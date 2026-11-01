//! `--config server.toml`: a flat TOML subset (`key = "string" | number`, `#` comments). Keys are the
//! long flag names without `--` (`listen`, `max-body`, `log-format`, ...). Unknown keys, duplicates,
//! tables, arrays, booleans and malformed lines are errors reported as `file:line: message`.
//! Command-line flags override the file.

/// Options that take a value (every `vcs-server` option except `--config` itself).
pub const KEYS: &[&str] = &[
    "root",
    "backend",
    "tokens",
    "listen",
    "audit-log",
    "audit-key",
    "rate-limit",
    "max-connections",
    "max-body",
    "policy",
    "max-lease-secs",
    "skew-epsilon-ms",
    "tls-cert",
    "tls-key",
    "tls-client-ca",
    "log-format",
    "log-level",
    "shutdown-grace",
    "metrics-token",
    "backend-retries",
    "backend-timeout",
    "breaker-threshold",
    "breaker-cooldown",
];

/// Parse `text` (named `file` in errors) into `(key, value)` pairs in file order.
pub fn parse(text: &str, file: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = vec![];
    for (i, raw) in text.lines().enumerate() {
        let at = |m: &str| format!("{file}:{}: {m}", i + 1);
        let line = if i == 0 { raw.strip_prefix('\u{feff}').unwrap_or(raw) } else { raw };
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            return Err(at("tables are not supported; use flat `key = value` lines"));
        }
        let (k, v) = line.split_once('=').ok_or_else(|| at("expected `key = value`"))?;
        let k = k.trim();
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            return Err(at("malformed key"));
        }
        let key = k.replace('_', "-");
        if !KEYS.contains(&key.as_str()) {
            return Err(at(&format!("unknown key `{k}`")));
        }
        if out.iter().any(|(e, _)| *e == key) {
            return Err(at(&format!("duplicate key `{k}`")));
        }
        let val = value(v.trim()).map_err(|m| at(&m))?;
        out.push((key, val));
    }
    Ok(out)
}

fn value(v: &str) -> Result<String, String> {
    if let Some(rest) = v.strip_prefix('"') {
        let mut s = String::new();
        let mut it = rest.chars();
        loop {
            match it.next() {
                None => return Err("unterminated string".into()),
                Some('"') => break,
                Some('\\') => match it.next() {
                    Some('"') => s.push('"'),
                    Some('\\') => s.push('\\'),
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    _ => return Err("unsupported escape in string".into()),
                },
                Some(c) if c.is_control() => return Err("control character in string".into()),
                Some(c) => s.push(c),
            }
        }
        let tail: String = it.collect();
        let tail = tail.trim();
        if !tail.is_empty() && !tail.starts_with('#') {
            return Err("unexpected text after string".into());
        }
        return Ok(s);
    }
    if let Some(rest) = v.strip_prefix('\'') {
        let (s, tail) = rest.split_once('\'').ok_or("unterminated string")?;
        let tail = tail.trim();
        if !tail.is_empty() && !tail.starts_with('#') {
            return Err("unexpected text after string".into());
        }
        return Ok(s.to_owned());
    }
    let bare = v.split('#').next().unwrap_or("").trim();
    if bare.is_empty() {
        return Err("missing value".into());
    }
    if matches!(bare, "true" | "false") || bare.starts_with(['[', '{']) {
        return Err("only strings and numbers are supported".into());
    }
    let num = bare.replace('_', "");
    if num.parse::<f64>().is_err() {
        return Err(format!("`{bare}` is neither a number nor a quoted string"));
    }
    Ok(num)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_reports_lines() {
        let t = "# c\nlisten = \"0.0.0.0:1\" # x\nmax_body = 1_000\n\nshutdown-grace = 2.5\nroot = 'a b'\n";
        let p = parse(t, "s.toml").unwrap();
        assert_eq!(
            p,
            vec![
                ("listen".into(), "0.0.0.0:1".into()),
                ("max-body".into(), "1000".into()),
                ("shutdown-grace".into(), "2.5".into()),
                ("root".into(), "a b".into())
            ]
        );
        for (t, want) in [
            ("a = 1", "s.toml:1: unknown key `a`"),
            ("\n\nlisten = ", "s.toml:3: missing value"),
            ("root = \"x\nroot = \"y\"", "s.toml:1: unterminated string"),
            ("root=\"a\"\nroot=\"b\"", "s.toml:2: duplicate key `root`"),
            ("[x]", "s.toml:1: tables"),
            ("root = true", "s.toml:1: only strings"),
            ("root = nope", "s.toml:1: `nope`"),
            ("root \"x\"", "s.toml:1: expected"),
        ] {
            let e = parse(t, "s.toml").unwrap_err();
            assert!(e.starts_with(want), "{t:?}: {e}");
        }
    }

    /// Deterministic fuzz: mutated and random inputs must give Ok or a `file:line:` error, never a panic.
    #[test]
    fn malformed_input_never_panics() {
        let seed_text = "listen = \"127.0.0.1:8480\"\nmax-body = 100\n# c\nroot = '/d'\n";
        let mut x = 0x1234_5678_9abc_def0u64;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let alphabet = b"=\"'#[]{}\\ \n\t\r-_.0aZ\xe2\x82\xac\xff\x00";
        for _ in 0..20_000 {
            let mut b = seed_text.as_bytes().to_vec();
            for _ in 0..(next() % 6) {
                let i = (next() as usize) % (b.len() + 1);
                match next() % 3 {
                    0 if i < b.len() => b[i] = alphabet[(next() as usize) % alphabet.len()],
                    1 => b.insert(i, alphabet[(next() as usize) % alphabet.len()]),
                    _ if i < b.len() => {
                        b.remove(i);
                    }
                    _ => {}
                }
            }
            let s = String::from_utf8_lossy(&b);
            if let Err(e) = parse(&s, "f") {
                assert!(e.starts_with("f:"), "{e}");
            }
        }
        for n in 0..2000 {
            let b: Vec<u8> = (0..(n % 64)).map(|_| next() as u8).collect();
            let _ = parse(&String::from_utf8_lossy(&b), "f");
        }
    }
}
