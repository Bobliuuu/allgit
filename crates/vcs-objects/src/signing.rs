//! Ed25519 signatures on commits and annotated tags (docs/signing.md).
//!
//! No format bump: a tag signs through its existing `signature` field; a commit has no such field,
//! so its signature is a trailer block at the end of the message. Either way the signature is part
//! of the object, so the object id covers it.
//!
//! The signed payload is `"allgit-signature-v1\n" || canonical object encoding with the signature
//! removed` (commit: message cut at the trailer; tag: empty `signature`). The kind byte inside the
//! encoding keeps commit and tag payloads apart.
//!
//! Signature text (both places): `ssh-ed25519 <base64 OpenSSH public key blob> <base64 64-byte sig>`.

use crate::object::{AnnotatedTag, Commit, Object};
use ed25519_dalek::Signer;

/// Domain-separation prefix of every signed payload.
pub const DOMAIN: &str = "allgit-signature-v1\n";
pub const KEY_TYPE: &str = "ssh-ed25519";
const TRAILER_BEGIN: &str = "\n-----BEGIN ALLGIT SIGNATURE-----\n";
const TRAILER_END: &str = "\n-----END ALLGIT SIGNATURE-----\n";

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignError {
    #[error("{0}")]
    Parse(String),
    #[error("the key is passphrase-protected; encrypted keys are not supported (use an unencrypted ed25519 key)")]
    Encrypted,
    #[error("unsupported key: {0} (only ssh-ed25519 is supported)")]
    Unsupported(String),
}

// ---------- base64 (standard alphabet, padded) ----------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(d: &[u8]) -> String {
    let mut o = String::with_capacity(d.len().div_ceil(3) * 4);
    for c in d.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                o.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                o.push('=');
            }
        }
    }
    o
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_end_matches('=');
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for ch in s.bytes() {
        let v = B64.iter().position(|&b| b == ch)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    // leftover bits must be zero padding
    (acc == 0).then_some(out)
}

// ---------- OpenSSH wire helpers ----------

struct Rd<'a>(&'a [u8]);
impl<'a> Rd<'a> {
    fn u32(&mut self) -> Option<u32> {
        let (h, t) = self.0.split_first_chunk::<4>()?;
        self.0 = t;
        Some(u32::from_be_bytes(*h))
    }
    fn string(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        if n > self.0.len() {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
}

fn put_string(o: &mut Vec<u8>, s: &[u8]) {
    o.extend_from_slice(&(s.len() as u32).to_be_bytes());
    o.extend_from_slice(s);
}

/// OpenSSH public key blob of a raw ed25519 key.
pub fn public_blob(pk: &[u8; 32]) -> Vec<u8> {
    let mut b = vec![];
    put_string(&mut b, KEY_TYPE.as_bytes());
    put_string(&mut b, pk);
    b
}

fn blob_to_pk(blob: &[u8]) -> Option<[u8; 32]> {
    let mut r = Rd(blob);
    (r.string()? == KEY_TYPE.as_bytes()).then_some(())?;
    let k = r.string()?;
    r.0.is_empty().then_some(())?;
    k.try_into().ok()
}

/// Short stable fingerprint of a public key blob (`B3:` + 16 hex digits of its BLAKE3 hash).
pub fn fingerprint(blob: &[u8]) -> String {
    let h = blake3::hash(blob);
    format!("B3:{}", &h.to_hex()[..16])
}

// ---------- keys ----------

pub struct SigningKey(ed25519_dalek::SigningKey);

impl SigningKey {
    pub fn from_seed(seed: [u8; 32]) -> Self {
        SigningKey(ed25519_dalek::SigningKey::from_bytes(&seed))
    }

    /// Parse an unencrypted OpenSSH ed25519 private key (`-----BEGIN OPENSSH PRIVATE KEY-----`).
    pub fn from_openssh(text: &str) -> Result<Self, SignError> {
        let bad = |m: &str| SignError::Parse(format!("bad OpenSSH private key: {m}"));
        let body: String = text
            .lines()
            .skip_while(|l| !l.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"))
            .skip(1)
            .take_while(|l| !l.starts_with("-----END"))
            .map(str::trim)
            .collect();
        if body.is_empty() {
            return Err(bad("no OPENSSH PRIVATE KEY block"));
        }
        let raw = b64_decode(&body).ok_or_else(|| bad("invalid base64"))?;
        let rest = raw.strip_prefix(b"openssh-key-v1\0").ok_or_else(|| bad("missing magic"))?;
        let mut r = Rd(rest);
        let cipher = r.string().ok_or_else(|| bad("truncated"))?;
        let kdf = r.string().ok_or_else(|| bad("truncated"))?;
        r.string().ok_or_else(|| bad("truncated"))?;
        if cipher != b"none" || kdf != b"none" {
            return Err(SignError::Encrypted);
        }
        if r.u32() != Some(1) {
            return Err(bad("expected exactly one key"));
        }
        r.string().ok_or_else(|| bad("truncated"))?; // public blob (re-derived below)
        let mut p = Rd(r.string().ok_or_else(|| bad("truncated"))?);
        let (c1, c2) = (p.u32().ok_or_else(|| bad("truncated"))?, p.u32().ok_or_else(|| bad("truncated"))?);
        if c1 != c2 {
            return Err(bad("check integers differ"));
        }
        let ty = p.string().ok_or_else(|| bad("truncated"))?;
        if ty != KEY_TYPE.as_bytes() {
            return Err(SignError::Unsupported(String::from_utf8_lossy(ty).into_owned()));
        }
        let pubk = p.string().ok_or_else(|| bad("truncated"))?;
        let sec = p.string().ok_or_else(|| bad("truncated"))?;
        if sec.len() != 64 || pubk.len() != 32 {
            return Err(bad("wrong key length"));
        }
        let k = SigningKey::from_seed(sec[..32].try_into().unwrap());
        if k.0.verifying_key().as_bytes()[..] != sec[32..] || sec[32..] != *pubk {
            return Err(bad("public half does not match the seed"));
        }
        Ok(k)
    }

    pub fn public_blob(&self) -> Vec<u8> {
        public_blob(self.0.verifying_key().as_bytes())
    }

    /// `ssh-ed25519 <base64>`: the allowed-signers key fields for this key.
    pub fn public_line(&self) -> String {
        format!("{KEY_TYPE} {}", b64_encode(&self.public_blob()))
    }

    fn sign_payload(&self, payload: &[u8]) -> String {
        let sig = self.0.sign(payload);
        format!("{KEY_TYPE} {} {}", b64_encode(&self.public_blob()), b64_encode(&sig.to_bytes()))
    }
}

// ---------- payloads ----------

fn payload(obj: &Object) -> Vec<u8> {
    let mut p = DOMAIN.as_bytes().to_vec();
    p.extend_from_slice(&obj.encode());
    p
}

/// Split a commit message into (signed message, signature text) at its trailer block.
pub fn split_commit_signature(msg: &str) -> (&str, Option<&str>) {
    if let Some(pos) = msg.rfind(TRAILER_BEGIN) {
        if let Some(inner) = msg[pos + TRAILER_BEGIN.len()..].strip_suffix(TRAILER_END) {
            if !inner.contains('\n') {
                return (&msg[..pos], Some(inner));
            }
        }
    }
    (msg, None)
}

/// Message without the signature trailer, for display.
pub fn strip_commit_signature(msg: &str) -> &str {
    split_commit_signature(msg).0
}

fn commit_payload(c: &Commit, base: &str) -> Vec<u8> {
    let mut c = c.clone();
    c.message = base.to_owned();
    payload(&Object::Commit(c))
}

fn tag_payload(t: &AnnotatedTag) -> Vec<u8> {
    let mut t = t.clone();
    t.signature = vec![];
    payload(&Object::Tag(t))
}

/// Return `c` signed: the trailer is appended to the message. Any existing trailer is replaced.
pub fn sign_commit(c: &Commit, key: &SigningKey) -> Commit {
    let base = split_commit_signature(&c.message).0.to_owned();
    let line = key.sign_payload(&commit_payload(c, &base));
    let mut out = c.clone();
    out.message = format!("{base}{TRAILER_BEGIN}{line}{TRAILER_END}");
    out
}

/// Signature bytes for an annotated tag (its `signature` field is ignored when signing).
pub fn sign_tag(t: &AnnotatedTag, key: &SigningKey) -> Vec<u8> {
    key.sign_payload(&tag_payload(t)).into_bytes()
}

// ---------- trust ----------

/// Allowed signers and revoked keys.
#[derive(Clone, Debug, Default)]
pub struct Trust {
    allowed: Vec<(String, Vec<u8>)>,
    revoked: Vec<Vec<u8>>,
}

fn parse_key_fields(f: &[&str]) -> Option<Vec<u8>> {
    if f.len() < 2 || f[0] != KEY_TYPE {
        return None;
    }
    let blob = b64_decode(f[1])?;
    blob_to_pk(&blob)?;
    Some(blob)
}

impl Trust {
    /// Lines `principal ssh-ed25519 base64key [comment]`; `#` starts a comment.
    pub fn parse_allowed(&mut self, text: &str) -> Result<(), String> {
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            let blob = f
                .get(1..)
                .and_then(parse_key_fields)
                .ok_or_else(|| format!("allowed signers line {}: expected `<principal> ssh-ed25519 <base64 key>`", n + 1))?;
            self.allowed.push((f[0].to_owned(), blob));
        }
        Ok(())
    }

    /// Revoked keys: lines `ssh-ed25519 base64key [comment]` (a leading principal is accepted
    /// too, so an allowed-signers line can be pasted in).
    pub fn parse_revoked(&mut self, text: &str) -> Result<(), String> {
        for (n, line) in text.lines().enumerate() {
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let f: Vec<&str> = line.split_whitespace().collect();
            let blob = parse_key_fields(&f)
                .or_else(|| f.get(1..).and_then(parse_key_fields))
                .ok_or_else(|| format!("revoked keys line {}: expected `ssh-ed25519 <base64 key>`", n + 1))?;
            self.revoked.push(blob);
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.allowed.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SigStatus {
    Unsigned,
    /// Valid signature by an allowed, unrevoked key.
    Good {
        principal: String,
        fingerprint: String,
    },
    /// Malformed, or the signature does not match the object.
    Bad(String),
    /// Valid signature, but the key is not in the allowed-signers list.
    UnknownKey {
        fingerprint: String,
    },
    /// Valid signature by a revoked key.
    Revoked {
        fingerprint: String,
    },
}

impl SigStatus {
    pub fn is_good(&self) -> bool {
        matches!(self, SigStatus::Good { .. })
    }
}

fn check(sigtext: &str, payload: &[u8], trust: &Trust) -> SigStatus {
    let f: Vec<&str> = sigtext.split(' ').collect();
    let (Some(blob), Some(sig)) = (f.get(..2).and_then(parse_key_fields), f.get(2).and_then(|s| b64_decode(s))) else {
        return SigStatus::Bad("malformed signature".into());
    };
    if f.len() != 3 {
        return SigStatus::Bad("malformed signature".into());
    }
    let (Some(pk), Ok(sig)) = (blob_to_pk(&blob), <[u8; 64]>::try_from(sig.as_slice())) else {
        return SigStatus::Bad("malformed signature".into());
    };
    let Ok(vk) = ed25519_dalek::VerifyingKey::from_bytes(&pk) else {
        return SigStatus::Bad("invalid public key".into());
    };
    if vk.verify_strict(payload, &ed25519_dalek::Signature::from_bytes(&sig)).is_err() {
        return SigStatus::Bad("signature does not match the object".into());
    }
    let fp = fingerprint(&blob);
    if trust.revoked.contains(&blob) {
        return SigStatus::Revoked { fingerprint: fp };
    }
    match trust.allowed.iter().find(|(_, k)| *k == blob) {
        Some((p, _)) => SigStatus::Good {
            principal: p.clone(),
            fingerprint: fp,
        },
        None => SigStatus::UnknownKey { fingerprint: fp },
    }
}

pub fn verify_commit(c: &Commit, trust: &Trust) -> SigStatus {
    match split_commit_signature(&c.message) {
        (_, None) => SigStatus::Unsigned,
        (base, Some(s)) => check(s, &commit_payload(c, base), trust),
    }
}

pub fn verify_tag(t: &AnnotatedTag, trust: &Trust) -> SigStatus {
    if t.signature.is_empty() {
        return SigStatus::Unsigned;
    }
    match std::str::from_utf8(&t.signature) {
        Ok(s) => check(s, &tag_payload(t), trust),
        Err(_) => SigStatus::Bad("not an allgit signature".into()),
    }
}

/// Fixed test keys (generated with ssh-keygen, no passphrase). Never use for real signing.
#[doc(hidden)]
pub mod fixtures {
    pub const KEY1: &str = include_str!("../tests/fixtures/key1");
    pub const KEY1_PUB: &str = include_str!("../tests/fixtures/key1.pub");
    pub const KEY2: &str = include_str!("../tests/fixtures/key2");
    pub const KEY2_PUB: &str = include_str!("../tests/fixtures/key2.pub");
    pub const KEY_ENCRYPTED: &str = include_str!("../tests/fixtures/key_encrypted");
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;
    use crate::{ChangeId, ObjectId, Signature};

    fn commit() -> Commit {
        Commit {
            zone: "z".into(),
            parents: vec![],
            root: ObjectId::from_bytes([7; 32]),
            change_id: ChangeId::from_bytes([1; 16]),
            author: Signature {
                name: "A".into(),
                email: "a@x".into(),
                timestamp: 5,
            },
            message: "hello\n".into(),
        }
    }

    fn trust(pairs: &[(&str, &str)]) -> Trust {
        let mut t = Trust::default();
        for (p, k) in pairs {
            t.parse_allowed(&format!("{p} {k}\n")).unwrap();
        }
        t
    }

    #[test]
    fn base64_roundtrip() {
        for n in 0..20 {
            let d: Vec<u8> = (0..n as u8).map(|i| i.wrapping_mul(37)).collect();
            assert_eq!(b64_decode(&b64_encode(&d)).unwrap(), d);
        }
        assert!(b64_decode("a$b=").is_none());
    }

    #[test]
    fn parses_ssh_keygen_key_and_matches_pub() {
        let k = SigningKey::from_openssh(KEY1).unwrap();
        let pubf: Vec<&str> = KEY1_PUB.split_whitespace().collect();
        assert_eq!(k.public_line(), format!("{} {}", pubf[0], pubf[1]));
        assert_eq!(SigningKey::from_openssh(KEY_ENCRYPTED).err(), Some(SignError::Encrypted));
        assert!(SigningKey::from_openssh("junk").is_err());
    }

    #[test]
    fn commit_sign_verify_and_statuses() {
        let k1 = SigningKey::from_openssh(KEY1).unwrap();
        let k2 = SigningKey::from_openssh(KEY2).unwrap();
        let c = commit();
        assert_eq!(verify_commit(&c, &Trust::default()), SigStatus::Unsigned);
        let s = sign_commit(&c, &k1);
        assert_ne!(Object::Commit(c.clone()).id(), Object::Commit(s.clone()).id());
        assert_eq!(strip_commit_signature(&s.message), "hello\n");
        let t = trust(&[("alice", &k1.public_line())]);
        assert_eq!(
            verify_commit(&s, &t),
            SigStatus::Good {
                principal: "alice".into(),
                fingerprint: fingerprint(&k1.public_blob())
            }
        );
        // unknown signer
        assert!(matches!(verify_commit(&sign_commit(&c, &k2), &t), SigStatus::UnknownKey { .. }));
        // revoked
        let mut tr = t.clone();
        tr.parse_revoked(&k1.public_line()).unwrap();
        assert!(matches!(verify_commit(&s, &tr), SigStatus::Revoked { .. }));
        // tampering with any signed field breaks it
        let mut m = s.clone();
        m.author.name = "B".into();
        assert!(matches!(verify_commit(&m, &t), SigStatus::Bad(_)));
        let mut m = s.clone();
        m.root = ObjectId::from_bytes([8; 32]);
        assert!(matches!(verify_commit(&m, &t), SigStatus::Bad(_)));
        let mut m = s.clone();
        m.message = m.message.replacen("hello", "hellp", 1);
        assert!(matches!(verify_commit(&m, &t), SigStatus::Bad(_)));
        // forged: claim alice's key but sign with k2's signature bytes
        let forged_line = {
            let l2 = sign_commit(&c, &k2).message;
            let sig2 = split_commit_signature(&l2).1.unwrap().split(' ').nth(2).unwrap().to_owned();
            format!("{} {} {}", KEY_TYPE, b64_encode(&k1.public_blob()), sig2)
        };
        let mut f = c.clone();
        f.message = format!("hello\n{TRAILER_BEGIN}{forged_line}{TRAILER_END}");
        assert!(matches!(verify_commit(&f, &t), SigStatus::Bad(_)));
        // garbage trailer content
        let mut g = c.clone();
        g.message = format!("hello\n{TRAILER_BEGIN}ssh-ed25519 AAAA zzzz{TRAILER_END}");
        assert!(matches!(verify_commit(&g, &t), SigStatus::Bad(_)));
    }

    #[test]
    fn tag_sign_verify() {
        let k1 = SigningKey::from_openssh(KEY1).unwrap();
        let t0 = AnnotatedTag {
            zone: "z".into(),
            name: "v1".into(),
            target: ObjectId::from_bytes([3; 32]),
            tagger: commit().author,
            message: "rel".into(),
            signature: vec![],
        };
        assert_eq!(verify_tag(&t0, &Trust::default()), SigStatus::Unsigned);
        let mut t = t0.clone();
        t.signature = sign_tag(&t0, &k1);
        let tr = trust(&[("alice", &k1.public_line())]);
        assert!(verify_tag(&t, &tr).is_good());
        let mut moved = t.clone();
        moved.target = ObjectId::from_bytes([4; 32]);
        assert!(matches!(verify_tag(&moved, &tr), SigStatus::Bad(_)));
        let mut old = t0.clone();
        old.signature = b"opaque legacy".to_vec();
        assert!(matches!(verify_tag(&old, &tr), SigStatus::Bad(_)));
        // a commit signature never validates as a tag signature (payload kind differs)
        let cs = sign_commit(&commit(), &k1);
        let mut cross = t0.clone();
        cross.signature = split_commit_signature(&cs.message).1.unwrap().as_bytes().to_vec();
        assert!(matches!(verify_tag(&cross, &tr), SigStatus::Bad(_)));
    }

    #[test]
    fn trust_parse_errors() {
        let mut t = Trust::default();
        assert!(t.parse_allowed("alice ssh-rsa AAAA").is_err());
        assert!(t.parse_allowed("# c\n\n").is_ok());
        assert!(t.parse_revoked("garbage").is_err());
    }
}
