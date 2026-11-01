//! Mail-format patches (`format-patch` / `am`): one message per commit.
//!
//! Output: `From <id> Mon Sep 17 00:00:00 2001` line, `From:`, `Date:` (RFC 2822, UTC),
//! `Subject: [PATCH i/n] ...`, a blank line, the rest of the message, `---`, a diffstat, the diff
//! and a `-- ` trailer. Non-ASCII text is written as raw UTF-8 with `Content-Type`/`8bit`
//! headers. Input accepts what git writes: RFC 2047 encoded headers, folded headers,
//! quoted-printable and base64 bodies, an in-body `From:`/`Date:` override.
//!
//! Dates are Unix timestamps in UTC; a parsed zone offset is applied and then dropped.

use crate::error::{msg, Result};
use vcs_objects::Signature;

const DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// (year, month 1-12, day 1-31) of a day count since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `Thu, 7 Apr 2005 15:13:13 +0000`.
pub fn format_date(ts: u64) -> String {
    let days = (ts / 86_400) as i64;
    let secs = ts % 86_400;
    let (y, m, d) = civil_from_days(days);
    let wd = DAYS[((days + 3).rem_euclid(7)) as usize]; // 1970-01-01 was a Thursday
    format!(
        "{wd}, {d} {} {y} {:02}:{:02}:{:02} +0000",
        MONTHS[(m - 1) as usize],
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

/// Parse an RFC 2822 (or `YYYY-MM-DD hh:mm:ss +zzzz`) date into a Unix timestamp.
pub fn parse_date(s: &str) -> Option<u64> {
    let s = s.trim();
    let s = s.split_once("(").map(|(a, _)| a).unwrap_or(s).trim();
    let toks: Vec<&str> = s.split_whitespace().collect();
    let (y, m, d, time, zone): (i64, u32, u32, &str, &str);
    if toks.first().is_some_and(|t| t.len() == 10 && t.as_bytes()[4] == b'-') {
        let mut dp = toks[0].split('-');
        y = dp.next()?.parse().ok()?;
        m = dp.next()?.parse().ok()?;
        d = dp.next()?.parse().ok()?;
        time = toks.get(1)?;
        zone = toks.get(2).copied().unwrap_or("+0000");
    } else {
        let mut i = 0;
        if toks.first()?.ends_with(',') {
            i = 1;
        }
        d = toks.get(i)?.parse().ok()?;
        let mon = toks.get(i + 1)?;
        m = MONTHS.iter().position(|x| mon.len() >= 3 && mon[..3].eq_ignore_ascii_case(x))? as u32 + 1;
        let mut yy: i64 = toks.get(i + 2)?.parse().ok()?;
        if yy < 100 {
            yy += if yy < 70 { 2000 } else { 1900 };
        }
        y = yy;
        time = toks.get(i + 3)?;
        zone = toks.get(i + 4).copied().unwrap_or("+0000");
    }
    let mut tp = time.split(':');
    let hh: i64 = tp.next()?.parse().ok()?;
    let mm: i64 = tp.next()?.parse().ok()?;
    let ss: i64 = tp.next().map(|x| x.parse().unwrap_or(0)).unwrap_or(0);
    let off: i64 = match zone {
        "GMT" | "UT" | "UTC" | "Z" => 0,
        z if (z.starts_with('+') || z.starts_with('-')) && z.len() == 5 => {
            let n: i64 = z[1..].parse().ok()?;
            let v = (n / 100) * 3600 + (n % 100) * 60;
            if z.starts_with('-') {
                -v
            } else {
                v
            }
        }
        _ => 0,
    };
    let t = days_from_civil(y, m, d) * 86_400 + hh * 3600 + mm * 60 + ss - off;
    u64::try_from(t).ok()
}

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn b64_decode(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for &c in s {
        if let Some(v) = b64_val(c) {
            acc = (acc << 6) | u32::from(v);
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
    }
    out
}

fn hex_val(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|v| v as u8)
}

/// Quoted-printable (`=XX`, soft line breaks). `q_header`: `_` means space (RFC 2047 Q).
fn qp_decode(s: &[u8], q_header: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'=' if i + 2 < s.len() && hex_val(s[i + 1]).is_some() && hex_val(s[i + 2]).is_some() => {
                out.push(hex_val(s[i + 1]).unwrap() * 16 + hex_val(s[i + 2]).unwrap());
                i += 3;
            }
            b'=' if s.get(i + 1) == Some(&b'\n') => i += 2,
            b'=' if s.get(i + 1) == Some(&b'\r') && s.get(i + 2) == Some(&b'\n') => i += 3,
            b'_' if q_header => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Decode RFC 2047 `=?charset?Q|B?text?=` words (UTF-8/ASCII/Latin-1 only; others are kept lossy).
pub fn decode_words(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    let mut last_encoded = false;
    while let Some(start) = rest.find("=?") {
        let (before, tail) = rest.split_at(start);
        let parsed = (|| {
            let t = &tail[2..];
            let (cs, t) = t.split_once('?')?;
            let (enc, t) = t.split_once('?')?;
            let (text, after) = t.split_once("?=")?;
            let bytes = match enc {
                "B" | "b" => b64_decode(text.as_bytes()),
                "Q" | "q" => qp_decode(text.as_bytes(), true),
                _ => return None,
            };
            let st = if cs.eq_ignore_ascii_case("iso-8859-1") || cs.eq_ignore_ascii_case("latin1") {
                bytes.iter().map(|&b| b as char).collect::<String>()
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            Some((st, after))
        })();
        match parsed {
            Some((st, after)) => {
                // whitespace between two encoded words is dropped
                if !(last_encoded && before.trim().is_empty()) {
                    out.push_str(before);
                }
                out.push_str(&st);
                rest = after;
                last_encoded = true;
            }
            None => {
                out.push_str(before);
                out.push_str("=?");
                rest = &tail[2..];
                last_encoded = false;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `Name <email>` -> (name, email).
pub fn split_address(s: &str) -> (String, String) {
    let s = s.trim();
    if let (Some(a), Some(b)) = (s.rfind('<'), s.rfind('>')) {
        if a < b {
            let name = s[..a].trim().trim_matches('"').trim().to_owned();
            let email = s[a + 1..b].trim().to_owned();
            return (if name.is_empty() { email.clone() } else { name }, email);
        }
    }
    (s.to_owned(), s.to_owned())
}

pub struct Mail {
    pub author: Signature,
    /// Commit message: subject, a blank line, body.
    pub message: String,
    /// Everything from the first diff header on.
    pub diff: Vec<u8>,
}

/// Split an mbox into messages. A message starts at a `From ` line that follows a blank line (or
/// the start) and carries a `hh:mm:ss` time, like git's own splitter.
pub fn split_mbox(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = vec![];
    let mut pos = 0;
    let mut prev_blank = true;
    while pos < data.len() {
        let end = data[pos..]
            .iter()
            .position(|&b| b == b'\n')
            .map(|e| pos + e + 1)
            .unwrap_or(data.len());
        let line = &data[pos..end];
        if prev_blank && line.starts_with(b"From ") && looks_like_from_line(line) {
            starts.push(pos);
        }
        prev_blank = line == b"\n" || line == b"\r\n";
        pos = end;
    }
    if starts.is_empty() {
        return if data.iter().all(|b| b.is_ascii_whitespace()) {
            vec![]
        } else {
            vec![data]
        };
    }
    let mut out = vec![];
    // text before the first From line is not a message
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(data.len());
        out.push(&data[s..e]);
    }
    out
}

fn looks_like_from_line(line: &[u8]) -> bool {
    let t = String::from_utf8_lossy(line);
    t.as_bytes().windows(8).any(|w| {
        w[2] == b':'
            && w[5] == b':'
            && w[..2].iter().all(u8::is_ascii_digit)
            && w[3..5].iter().all(u8::is_ascii_digit)
            && w[6..8].iter().all(u8::is_ascii_digit)
    })
}

/// Parse one message.
pub fn parse_mail(raw: &[u8]) -> Result<Mail> {
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut s = 0;
    for (i, b) in raw.iter().enumerate() {
        if *b == b'\n' {
            lines.push(&raw[s..=i]);
            s = i + 1;
        }
    }
    if s < raw.len() {
        lines.push(&raw[s..]);
    }
    let text = |l: &[u8]| String::from_utf8_lossy(l).trim_end_matches(['\r', '\n']).to_owned();
    let mut i = 0;
    if lines.first().is_some_and(|l| l.starts_with(b"From ")) {
        i = 1;
    }
    // headers (with folding) up to the first blank line
    let mut headers: Vec<(String, String)> = Vec::new();
    while i < lines.len() {
        let t = text(lines[i]);
        if t.is_empty() {
            i += 1;
            break;
        }
        if t.starts_with(' ') || t.starts_with('\t') {
            if let Some(last) = headers.last_mut() {
                last.1.push(' ');
                last.1.push_str(t.trim());
            }
        } else if let Some((k, v)) = t.split_once(':') {
            headers.push((k.to_ascii_lowercase(), v.trim().to_owned()));
        } else {
            return msg(format!("not a mail message: header line {t:?}"));
        }
        i += 1;
    }
    let h = |k: &str| headers.iter().find(|x| x.0 == k).map(|x| x.1.clone());
    let cte = h("content-transfer-encoding").unwrap_or_default().to_ascii_lowercase();
    let mut body: Vec<u8> = Vec::new();
    for l in &lines[i.min(lines.len())..] {
        body.extend_from_slice(l);
    }
    let body = match cte.as_str() {
        "quoted-printable" => qp_decode(&body, false),
        "base64" => b64_decode(&body),
        _ => body,
    };
    let from = h("from").map(|f| decode_words(&f));
    let date = h("date");
    let subject_raw = h("subject").map(|f| decode_words(&f)).unwrap_or_default();
    let subject = clean_subject(&subject_raw);
    // body: in-body From:/Date: overrides, message up to `---`, diff from the first diff header
    let blines: Vec<&[u8]> = {
        let mut v = Vec::new();
        let mut s = 0;
        for (k, b) in body.iter().enumerate() {
            if *b == b'\n' {
                v.push(&body[s..=k]);
                s = k + 1;
            }
        }
        if s < body.len() {
            v.push(&body[s..]);
        }
        v
    };
    let mut j = 0;
    let mut from_override = None;
    let mut date_override = None;
    while j < blines.len() && text(blines[j]).is_empty() && from_override.is_none() {
        j += 1;
    }
    while j < blines.len() {
        let t = text(blines[j]);
        if let Some(v) = t.strip_prefix("From: ") {
            from_override = Some(decode_words(v));
        } else if let Some(v) = t.strip_prefix("Date: ") {
            date_override = Some(v.to_owned());
        } else {
            break;
        }
        j += 1;
    }
    if from_override.is_some() {
        while j < blines.len() && text(blines[j]).is_empty() {
            j += 1;
        }
    }
    let mut msg_lines: Vec<String> = Vec::new();
    let mut diff_at = None;
    let mut k = j;
    let mut in_msg = true;
    while k < blines.len() {
        let l = blines[k];
        let t = text(l);
        let is_diff = l.starts_with(b"diff --git ")
            || l.starts_with(b"diff --vcs ")
            || (l.starts_with(b"--- ") && blines.get(k + 1).is_some_and(|n| n.starts_with(b"+++ ")))
            || l.starts_with(b"Index: ");
        if is_diff {
            diff_at = Some(k);
            break;
        }
        if in_msg && t == "---" {
            in_msg = false;
        }
        if in_msg {
            msg_lines.push(t);
        }
        k += 1;
    }
    let Some(d) = diff_at else {
        return msg(format!("patch {subject:?} contains no diff"));
    };
    let mut diff = Vec::new();
    for l in &blines[d..] {
        diff.extend_from_slice(l);
    }
    let mut message = subject.clone();
    let rest = msg_lines.join("\n");
    let rest = rest.trim_matches('\n').trim_end();
    if !rest.is_empty() {
        message.push_str("\n\n");
        message.push_str(rest);
    }
    if message.trim().is_empty() {
        return msg("patch has an empty subject and message");
    }
    let from = from_override
        .or(from)
        .ok_or_else(|| crate::Error::Msg("patch has no From: header".into()))?;
    let (name, email) = split_address(&from);
    let ts = date_override
        .or(date)
        .and_then(|d| parse_date(&d))
        .ok_or_else(|| crate::Error::Msg("patch has no valid Date: header".into()))?;
    Ok(Mail {
        author: Signature {
            name,
            email,
            timestamp: ts,
        },
        message,
        diff,
    })
}

/// Drop `Re:` and leading `[PATCH ...]` tags.
fn clean_subject(s: &str) -> String {
    let mut t = s.trim();
    loop {
        if let Some(r) = t.strip_prefix("Re:").or_else(|| t.strip_prefix("re:")) {
            t = r.trim_start();
        } else if t.starts_with('[') {
            match t.find(']') {
                Some(e) if t[..e].to_ascii_uppercase().contains("PATCH") => t = t[e + 1..].trim_start(),
                _ => break,
            }
        } else {
            break;
        }
    }
    t.to_owned()
}

/// Header for one message.
pub fn format_header(id_hex: &str, author: &Signature, subject_prefix: &str, subject: &str) -> String {
    let mut s = format!("From {:0<40} Mon Sep 17 00:00:00 2001\n", &id_hex[..id_hex.len().min(40)]);
    s.push_str(&format!("From: {} <{}>\n", author.name, author.email));
    s.push_str(&format!("Date: {}\n", format_date(author.timestamp)));
    if !(s.is_ascii() && subject.is_ascii()) {
        s.push_str("MIME-Version: 1.0\nContent-Type: text/plain; charset=UTF-8\nContent-Transfer-Encoding: 8bit\n");
    }
    s.push_str(&format!("Subject: {subject_prefix}{subject}\n"));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_roundtrip() {
        for ts in [0u64, 1_112_886_793, 1_700_000_000, 4_102_444_800] {
            assert_eq!(parse_date(&format_date(ts)), Some(ts), "{}", format_date(ts));
        }
        assert_eq!(format_date(1_112_886_793), "Thu, 7 Apr 2005 15:13:13 +0000");
        assert_eq!(parse_date("Thu, 7 Apr 2005 08:13:13 -0700"), Some(1_112_886_793));
    }

    #[test]
    fn rfc2047_and_address() {
        assert_eq!(decode_words("=?UTF-8?q?J=C3=BCrgen_M?= <j@x>"), "Jürgen M <j@x>");
        assert_eq!(decode_words("=?utf-8?B?SsO8cmdlbg==?="), "Jürgen");
        assert_eq!(split_address("A B <a@b>"), ("A B".into(), "a@b".into()));
    }

    #[test]
    fn mail_roundtrip_and_split() {
        let a = Signature {
            name: "Ann".into(),
            email: "ann@x".into(),
            timestamp: 1_700_000_000,
        };
        let mut m = format_header(&"ab".repeat(32), &a, "[PATCH 1/2] ", "do it");
        m.push_str(
            "\nbody line\n---\n f | 1 +\n 1 file changed\n\ndiff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n-- \nvcs\n\n",
        );
        let two = format!("{m}{m}");
        let parts = split_mbox(two.as_bytes());
        assert_eq!(parts.len(), 2);
        let p = parse_mail(parts[1]).unwrap();
        assert_eq!(p.author, a);
        assert_eq!(p.message, "do it\n\nbody line");
        assert!(p.diff.starts_with(b"diff --git"));
    }
}
