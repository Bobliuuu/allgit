//! Append-only, hash-chained audit log (JSON Lines).
//!
//! One record per authenticated mutating request (every `PUT` and `DELETE`, accepted or rejected):
//!
//! ```text
//! {"seq":0,"ts_ms":1700000000000,"principal":"alice","op":"PUT","key":"manifest","outcome":"200","prev":"<64 hex>","hash":"<64 hex>"}
//! ```
//!
//! `hash` is BLAKE3 of the line up to and including the `prev` field (so it covers `seq`, the
//! timestamp, the request and the previous hash); the first record's `prev` is 64 zeros. With a
//! 32-byte key the hash is a keyed BLAKE3 MAC instead, which also stops someone who can write the
//! file but does not hold the key from recomputing the chain.
//!
//! What this detects: edited, reordered, inserted or deleted records in the middle of the file, and a
//! truncated final line. What it cannot detect on its own: removal of the newest records (the chain
//! just ends earlier) or a full rewrite by someone holding the key (or, unkeyed, anyone who can
//! write the file). Anchor the head hash that `audit verify` prints somewhere else for that.
//! The file is opened append-only by this process; making it immutable on disk (`chattr +a`,
//! WORM storage) is the operator's job.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::sync::Mutex;

pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, PartialEq, Eq)]
pub struct VerifyError {
    /// 1-based line number.
    pub line: usize,
    pub msg: String,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.line, self.msg)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct VerifyReport {
    pub records: u64,
    /// Hash of the newest record (GENESIS for an empty log).
    pub head: String,
}

pub fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 || c == '\u{7f}' => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn digest(key: Option<&[u8; 32]>, body: &[u8]) -> String {
    match key {
        Some(k) => blake3::keyed_hash(k, body).to_hex().to_string(),
        None => blake3::hash(body).to_hex().to_string(),
    }
}

/// Parse a 64-hex key file's text.
pub fn parse_key(text: &str) -> Result<[u8; 32], String> {
    let t = text.trim();
    if t.len() != 64 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("audit key must be 64 hex characters".into());
    }
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = u8::from_str_radix(&t[2 * i..2 * i + 2], 16).map_err(|e| e.to_string())?;
    }
    Ok(k)
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Verify a log file: every line must parse, carry the next `seq`, point at the previous hash, and hash
/// correctly. Returns the record count and head hash.
pub fn verify_file(path: &Path, key: Option<&[u8; 32]>) -> Result<VerifyReport, VerifyError> {
    let f = File::open(path).map_err(|e| VerifyError {
        line: 0,
        msg: format!("cannot open {}: {e}", path.display()),
    })?;
    let mut r = BufReader::new(f);
    let mut prev = GENESIS.to_owned();
    let mut n = 0u64;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let got = r.read_until(b'\n', &mut buf).map_err(|e| VerifyError {
            line: n as usize + 1,
            msg: e.to_string(),
        })?;
        if got == 0 {
            break;
        }
        let line_no = n as usize + 1;
        let bad = |m: &str| VerifyError {
            line: line_no,
            msg: m.to_owned(),
        };
        if buf.last() != Some(&b'\n') {
            return Err(bad("truncated final record (no newline)"));
        }
        buf.pop();
        let line = std::str::from_utf8(&buf).map_err(|_| bad("not UTF-8"))?;
        let (body, hash) = line
            .rsplit_once(",\"hash\":\"")
            .and_then(|(b, h)| h.strip_suffix("\"}").map(|h| (b, h)))
            .ok_or_else(|| bad("malformed record"))?;
        if !is_hex64(hash) {
            return Err(bad("malformed hash field"));
        }
        let want_prefix = format!("{{\"seq\":{n},");
        if !body.starts_with(&want_prefix) {
            return Err(bad("sequence number out of order (record inserted, deleted or reordered)"));
        }
        let rec_prev = body
            .strip_suffix('"')
            .and_then(|b| b.rsplit_once(",\"prev\":\""))
            .map(|(_, p)| p)
            .ok_or_else(|| bad("malformed prev field"))?;
        if rec_prev != prev {
            return Err(bad("previous-hash does not match the preceding record (chain broken)"));
        }
        if digest(key, body.as_bytes()) != hash {
            return Err(bad("record hash mismatch (record was modified)"));
        }
        prev = hash.to_owned();
        n += 1;
    }
    Ok(VerifyReport { records: n, head: prev })
}

/// Decision detail of a record. `reason` is one of `accepted`, `policy`, `lock`, `lock-required`,
/// `invalid`, `forbidden`, `unavailable`, `conflict`, `error`; `policy` is the hex BLAKE3 of the
/// path policy in force; `vtime` is the validator's server time (ms) for an accepted manifest write, which is
/// also the `time_hw` stored in the same compare-and-swap.
#[derive(Clone, Debug, Default)]
pub struct Extra {
    pub reason: Option<&'static str>,
    pub policy: Option<String>,
    pub vtime: Option<u64>,
}

struct State {
    file: File,
    seq: u64,
    prev: String,
}

pub struct AuditLog {
    path: std::path::PathBuf,
    state: Mutex<State>,
    key: Option<[u8; 32]>,
}

impl AuditLog {
    /// Open (creating if needed) and verify the existing contents; refuses to append to a log whose
    /// chain does not verify.
    pub fn open(path: &Path, key: Option<[u8; 32]>) -> std::io::Result<Self> {
        let (seq, prev) = if path.exists() {
            let rep = verify_file(path, key.as_ref())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("existing audit log does not verify: {e}")))?;
            (rep.records, rep.head)
        } else {
            (0, GENESIS.to_owned())
        };
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            path: path.to_path_buf(),
            state: Mutex::new(State { file, seq, prev }),
            key,
        })
    }

    /// Up to `limit` raw records with `seq > after` (or from the start when `after` is `None`), as
    /// JSON lines, plus the newest returned seq and the head hash. Read under the append lock so a
    /// half-written line is never returned.
    pub fn page(&self, after: Option<u64>, limit: usize) -> std::io::Result<(Vec<String>, Option<u64>, String)> {
        let st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let head = st.prev.clone();
        let r = BufReader::new(File::open(&self.path)?);
        let mut out = Vec::new();
        let mut last = None;
        for (n, line) in r.lines().enumerate() {
            let n = n as u64;
            if after.is_some_and(|a| n <= a) {
                continue;
            }
            if out.len() >= limit {
                break;
            }
            out.push(line?);
            last = Some(n);
        }
        Ok((out, last, head))
    }

    /// fsync the file (records are already synced on append; this is the shutdown belt-and-braces).
    pub fn sync(&self) -> std::io::Result<()> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).file.sync_all()
    }

    /// Append one record and flush it to disk. Returns the record's hash.
    pub fn record(&self, ts_ms: u64, principal: &str, op: &str, key: &str, outcome: &str) -> std::io::Result<String> {
        self.record_ext(ts_ms, principal, op, key, outcome, &Extra::default())
    }

    /// [`record`](Self::record) with the decision detail (`reason`, policy hash, validator time).
    pub fn record_ext(&self, ts_ms: u64, principal: &str, op: &str, key: &str, outcome: &str, extra: &Extra) -> std::io::Result<String> {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut ext = String::new();
        if let Some(r) = extra.reason {
            ext.push_str(&format!(",\"reason\":{}", json_str(r)));
        }
        if let Some(p) = &extra.policy {
            ext.push_str(&format!(",\"policy\":{}", json_str(p)));
        }
        if let Some(t) = extra.vtime {
            ext.push_str(&format!(",\"vtime\":{t}"));
        }
        let body = format!(
            "{{\"seq\":{},\"ts_ms\":{ts_ms},\"principal\":{},\"op\":{},\"key\":{},\"outcome\":{}{ext},\"prev\":\"{}\"",
            st.seq,
            json_str(principal),
            json_str(op),
            json_str(key),
            json_str(outcome),
            st.prev
        );
        let hash = digest(self.key.as_ref(), body.as_bytes());
        let line = format!("{body},\"hash\":\"{hash}\"}}\n");
        st.file.write_all(line.as_bytes())?;
        st.file.sync_data()?;
        st.seq += 1;
        st.prev = hash.clone();
        Ok(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, std::path::PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("audit.jsonl");
        (d, p)
    }

    fn fill(p: &Path, key: Option<[u8; 32]>) {
        let l = AuditLog::open(p, key).unwrap();
        for i in 0..5 {
            l.record(1000 + i, "alice", "PUT", &format!("objects/k{i}"), "201").unwrap();
        }
    }

    #[test]
    fn chain_verifies_and_resumes_after_reopen() {
        let (_d, p) = tmp();
        fill(&p, None);
        let r = verify_file(&p, None).unwrap();
        assert_eq!(r.records, 5);
        let l = AuditLog::open(&p, None).unwrap();
        l.record(2000, "bob", "PUT", "manifest", "403").unwrap();
        assert_eq!(verify_file(&p, None).unwrap().records, 6);
        assert_eq!(verify_file(&tmp().1, None).unwrap_err().line, 0, "missing file is an error");
    }

    #[test]
    fn detects_edit_delete_insert_reorder_truncation() {
        let (_d, p) = tmp();
        fill(&p, None);
        let text = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        let write = |ls: Vec<String>| {
            let mut s = ls.join("\n");
            s.push('\n');
            std::fs::write(&p, s).unwrap();
        };
        let owned = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // edit a field
        let mut l = owned(&lines);
        l[2] = l[2].replace("\"alice\"", "\"mallory\"");
        write(l);
        assert_eq!(verify_file(&p, None).unwrap_err().line, 3);
        // delete the middle
        let mut l = owned(&lines);
        l.remove(2);
        write(l);
        assert_eq!(verify_file(&p, None).unwrap_err().line, 3);
        // swap two
        let mut l = owned(&lines);
        l.swap(1, 2);
        write(l);
        assert!(verify_file(&p, None).is_err());
        // duplicate a record
        let mut l = owned(&lines);
        l.insert(2, lines[1].to_owned());
        write(l);
        assert!(verify_file(&p, None).is_err());
        // truncated last line
        std::fs::write(&p, text.trim_end_matches('\n')).unwrap();
        assert_eq!(verify_file(&p, None).unwrap_err().line, 5);
        // garbage
        std::fs::write(&p, "not json\n").unwrap();
        assert!(verify_file(&p, None).is_err());
        // restored original verifies again
        std::fs::write(&p, &text).unwrap();
        assert_eq!(verify_file(&p, None).unwrap().records, 5);
        // dropping the newest record is NOT detectable (documented limitation); the head changes.
        let head = verify_file(&p, None).unwrap().head;
        write(owned(&lines[..4]));
        assert_ne!(verify_file(&p, None).unwrap().head, head);
    }

    #[test]
    fn open_refuses_tampered_log_and_keyed_chain_resists_rewrite() {
        let (_d, p) = tmp();
        fill(&p, None);
        let t = std::fs::read_to_string(&p).unwrap().replace("objects/k1", "objects/zz");
        std::fs::write(&p, t).unwrap();
        assert!(AuditLog::open(&p, None).is_err());

        let (_d2, p2) = tmp();
        let key = [7u8; 32];
        fill(&p2, Some(key));
        assert!(verify_file(&p2, Some(&key)).is_ok());
        assert!(verify_file(&p2, None).is_err(), "unkeyed verification of a keyed log fails");
        assert!(verify_file(&p2, Some(&[8u8; 32])).is_err());
    }

    #[test]
    fn escaping_cannot_forge_fields() {
        let (_d, p) = tmp();
        let l = AuditLog::open(&p, None).unwrap();
        l.record(1, "a\"b,\"hash\":\"x", "PUT", "k\n\u{1}\"", "201").unwrap();
        l.record(2, "ünï", "PUT", "k", "201").unwrap();
        assert_eq!(verify_file(&p, None).unwrap().records, 2);
        assert_eq!(std::fs::read_to_string(&p).unwrap().lines().count(), 2);
    }

    #[test]
    fn extra_fields_are_chained_and_paged() {
        let (_d, p) = tmp();
        let l = AuditLog::open(&p, None).unwrap();
        let ex = Extra {
            reason: Some("policy"),
            policy: Some("ab".repeat(32)),
            vtime: Some(42),
        };
        l.record_ext(1, "a", "PUT", "manifest", "403", &ex).unwrap();
        l.record(2, "b", "PUT", "x", "201").unwrap();
        l.record(3, "c", "PUT", "y", "201").unwrap();
        assert_eq!(verify_file(&p, None).unwrap().records, 3);
        let (all, last, head) = l.page(None, 10).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all[0].contains("\"reason\":\"policy\"") && all[0].contains("\"vtime\":42"));
        assert_eq!(last, Some(2));
        assert_eq!(head, verify_file(&p, None).unwrap().head);
        let (pg, last, _) = l.page(None, 2).unwrap();
        assert_eq!((pg.len(), last), (2, Some(1)));
        let (pg, last, _) = l.page(Some(1), 2).unwrap();
        assert_eq!((pg.len(), last), (1, Some(2)));
        assert_eq!(l.page(Some(2), 2).unwrap().1, None);
        // editing the new fields breaks the chain
        let t = std::fs::read_to_string(&p).unwrap().replace("\"vtime\":42", "\"vtime\":43");
        std::fs::write(&p, t).unwrap();
        assert!(verify_file(&p, None).is_err());
    }

    #[test]
    fn parse_key_validates() {
        assert!(parse_key(&"ab".repeat(32)).is_ok());
        assert!(parse_key("short").is_err());
        assert!(parse_key(&"zz".repeat(32)).is_err());
    }
}
