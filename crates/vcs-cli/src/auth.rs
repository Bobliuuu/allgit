//! Client-side authentication for `http://` (vcs-server) remotes.
//!
//! Tokens are obtained through one path: `VCS_TOKEN` first, then the credential helper named by
//! `credential.helper` (default `file`). A token is never part of a URL, `.vcs/remote`, an error
//! message or debug output: [`Token`] prints as `<redacted>` and errors are scrubbed.
//!
//! Helper protocol (git-credential style): `vcs credential get|store|erase` reads `key=value`
//! lines on stdin (ended by a blank line or EOF) and, for `get`, writes `host=` and `token=` lines
//! to stdout (nothing when no credential is stored). Keys: `host` (`host:port`, or give `url=`),
//! `token`. `credential.helper` is one of:
//!   `file`      `~/.config/vcs/credentials` (mode 0600, looser modes are refused)
//!   `keychain`  macOS Keychain via the `security` command
//!   `!command`  run through `sh -c "command <op>"`   (user config only, never repo config)
//!   `/path/to/helper`  run as `helper <op>`            (user config only)
use crate::config::{self, Config, Source};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use vcs_repo::Error;

fn msg<T>(s: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(s.into()))
}

/// A bearer token. `Debug`/`Display` never show the value.
#[derive(Clone)]
pub struct Token(String);

impl Token {
    pub fn new(s: &str) -> Result<Token, Error> {
        let s = s.trim();
        if s.is_empty() {
            return msg("empty token");
        }
        if s.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return msg("a token cannot contain whitespace or control characters");
        }
        Ok(Token(s.to_owned()))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }
    /// Replace the token in `s`.
    pub fn scrub(&self, s: &str) -> String {
        s.replace(&self.0, "<redacted>")
    }
}

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}
impl std::fmt::Display for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// `http(s)://Host:8480/x` -> `host:8480` (the scheme is not part of the key). Only vcs-server remotes carry tokens.
pub fn host_key(url: &str) -> Result<String, Error> {
    let Some(rest) = url.strip_prefix("http://").or_else(|| url.strip_prefix("https://")) else {
        return msg(format!(
            "{url}: only http:// and https:// (vcs-server) remotes use tokens; local, file:// and s3:// remotes need no `vcs login`"
        ));
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(""); // drop any userinfo
    check_host(&host.to_ascii_lowercase())
}

fn check_host(h: &str) -> Result<String, Error> {
    if h.is_empty()
        || h.chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '=' | '"' | '\'' | '\\'))
    {
        return msg("invalid remote host");
    }
    Ok(h.to_owned())
}

// ---- key=value wire format ----

pub type Kv = BTreeMap<String, String>;

pub fn parse_kv(text: &str) -> Kv {
    let mut m = Kv::new();
    for l in text.lines() {
        let l = l.trim_end_matches('\r');
        if l.is_empty() {
            break;
        }
        if let Some((k, v)) = l.split_once('=') {
            m.insert(k.to_owned(), v.to_owned());
        }
    }
    m
}

pub fn render_kv(m: &Kv) -> String {
    m.iter().map(|(k, v)| format!("{k}={v}\n")).collect()
}

fn kv_host(m: &Kv) -> Result<String, Error> {
    if let Some(h) = m.get("host") {
        return check_host(&h.to_ascii_lowercase());
    }
    match m.get("url") {
        Some(u) => host_key(u),
        None => msg("credential input needs host= (or url=)"),
    }
}

// ---- helper selection ----

#[derive(Debug, PartialEq, Eq)]
pub enum Helper {
    File,
    Keychain,
    Shell(String),
    Program(String),
}

impl Helper {
    pub fn parse(spec: &str, source: Source) -> Result<Helper, Error> {
        let spec = spec.trim();
        let h = match spec {
            "file" => return Ok(Helper::File),
            "keychain" => return Ok(Helper::Keychain),
            s if s.starts_with('!') && s.len() > 1 => Helper::Shell(s[1..].to_owned()),
            s if s.starts_with('/') => Helper::Program(s.to_owned()),
            _ => {
                return msg(format!(
                    "unknown credential.helper '{spec}'\n\nUse: file, keychain, !command, or an absolute program path"
                ))
            }
        };
        if source == Source::Repo {
            return msg("credential.helper from a repository config can only be 'file' or 'keychain' (running commands from a repository config is refused); set it with --user");
        }
        Ok(h)
    }
    pub fn label(&self) -> String {
        match self {
            Helper::File => "file".into(),
            Helper::Keychain => "keychain".into(),
            Helper::Shell(c) => format!("!{c}"),
            Helper::Program(p) => p.clone(),
        }
    }
}

pub fn configured_helper(cfg: &Config) -> Result<Helper, Error> {
    match cfg.get_with_source("credential.helper") {
        None => Ok(Helper::File),
        Some((v, src)) => Helper::parse(&v, src),
    }
}

pub fn credentials_path() -> Result<PathBuf, Error> {
    match config::user_path() {
        Some(p) => Ok(p.with_file_name("credentials")),
        None => msg("cannot locate the user config directory (set HOME or VCS_CONFIG_HOME)"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Get,
    Store,
    Erase,
}

impl Op {
    pub fn name(self) -> &'static str {
        match self {
            Op::Get => "get",
            Op::Store => "store",
            Op::Erase => "erase",
        }
    }
}

/// Run one operation on a helper. `get` returns the token if one is stored.
pub fn run_helper(h: &Helper, op: Op, input: &Kv) -> Result<Option<Token>, Error> {
    let host = kv_host(input)?;
    let token = input.get("token").map(|t| Token::new(t)).transpose()?;
    match (h, op) {
        (Helper::File, _) => {
            let p = credentials_path()?;
            match op {
                Op::Get => file_get(&p, &host),
                Op::Store => file_store(&p, &host, need_token(&token)?).map(|_| None),
                Op::Erase => file_erase(&p, &host).map(|_| None),
            }
        }
        #[cfg(target_os = "macos")]
        (Helper::Keychain, _) => {
            let k = keychain::Keychain::system();
            match op {
                Op::Get => k.get(&host),
                Op::Store => k.store(&host, need_token(&token)?).map(|_| None),
                Op::Erase => k.erase(&host).map(|_| None),
            }
        }
        #[cfg(not(target_os = "macos"))]
        (Helper::Keychain, _) => msg("the keychain helper is only available on macOS; use credential.helper = file"),
        (Helper::Shell(c), _) => run_external(Command::new("sh").arg("-c").arg(format!("{c} {}", op.name())), op, &host, &token),
        (Helper::Program(p), _) => run_external(Command::new(p).arg(op.name()), op, &host, &token),
    }
}

fn need_token(t: &Option<Token>) -> Result<&Token, Error> {
    t.as_ref().ok_or_else(|| Error::Msg("credential store needs token=".into()))
}

fn run_external(cmd: &mut Command, op: Op, host: &str, token: &Option<Token>) -> Result<Option<Token>, Error> {
    let mut input = Kv::new();
    input.insert("host".into(), host.to_owned());
    if let Some(t) = token {
        input.insert("token".into(), t.expose().to_owned());
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Msg(format!("could not start credential helper: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped");
    let data = render_kv(&input) + "\n";
    // Feed from a thread so a helper that writes before reading cannot deadlock us.
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(data.as_bytes());
    });
    let out = child
        .wait_with_output()
        .map_err(|e| Error::Msg(format!("credential helper failed: {e}")))?;
    let _ = writer.join();
    let scrub = |s: &str| token.as_ref().map_or_else(|| s.to_owned(), |t| t.scrub(s));
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return msg(format!("credential helper exited with {}: {}", out.status, scrub(err.trim())));
    }
    if op != Op::Get {
        return Ok(None);
    }
    let reply = parse_kv(&String::from_utf8_lossy(&out.stdout));
    reply.get("token").map(|t| Token::new(t)).transpose()
}

// ---- file store ----

fn io_err(p: &Path) -> impl Fn(std::io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: p.to_owned(),
        source,
    }
}

fn read_creds(p: &Path) -> Result<BTreeMap<String, String>, Error> {
    let mut f = match std::fs::File::open(p) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(io_err(p)(e)),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = f.metadata().map_err(io_err(p))?.permissions().mode() & 0o777;
        if mode & 0o077 != 0 {
            return msg(format!(
                "{} has mode {mode:03o}; refusing to read credentials that other users can access\n\nFix it with:\n  chmod 600 {}",
                p.display(),
                p.display()
            ));
        }
    }
    let mut text = String::new();
    f.read_to_string(&mut text).map_err(io_err(p))?;
    let mut m = BTreeMap::new();
    for l in text.lines() {
        if let Some((h, t)) = l.split_once(' ') {
            m.insert(h.to_owned(), t.to_owned());
        }
    }
    Ok(m)
}

fn write_creds(p: &Path, m: &BTreeMap<String, String>) -> Result<(), Error> {
    let dir = p.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(io_err(dir))?;
    let tmp = p.with_extension(format!("tmp{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let body: String = m.iter().map(|(h, t)| format!("{h} {t}\n")).collect();
    let res = (|| {
        let mut f = o.open(&tmp)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, p)
    })();
    if let Err(e) = res {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err(p)(e));
    }
    Ok(())
}

pub fn file_get(p: &Path, host: &str) -> Result<Option<Token>, Error> {
    read_creds(p)?.get(host).map(|t| Token::new(t)).transpose()
}

pub fn file_store(p: &Path, host: &str, t: &Token) -> Result<(), Error> {
    let mut m = read_creds(p)?;
    m.insert(host.to_owned(), t.expose().to_owned());
    write_creds(p, &m)
}

pub fn file_erase(p: &Path, host: &str) -> Result<bool, Error> {
    let mut m = read_creds(p)?;
    let had = m.remove(host).is_some();
    if had {
        write_creds(p, &m)?;
    }
    Ok(had)
}

// ---- macOS keychain ----

#[cfg(any(target_os = "macos", test))]
pub mod keychain {
    use super::*;
    use std::ffi::OsString;

    /// Talks to the `security` command. `path` overrides PATH for the lookup (used by tests).
    pub struct Keychain {
        path: Option<OsString>,
        /// A specific keychain file instead of the default search list (tests use a temporary one).
        keychain: Option<std::path::PathBuf>,
    }

    impl Keychain {
        #[cfg(target_os = "macos")]
        pub fn system() -> Keychain {
            Keychain {
                path: None,
                keychain: None,
            }
        }
        #[cfg(test)]
        pub fn with_path(path: OsString) -> Keychain {
            Keychain {
                path: Some(path),
                keychain: None,
            }
        }
        #[cfg(all(test, target_os = "macos"))]
        pub fn with_keychain(file: std::path::PathBuf) -> Keychain {
            Keychain {
                path: None,
                keychain: Some(file),
            }
        }
        /// Trailing `security` argument naming the keychain file, if one was chosen.
        fn kc_arg(&self) -> String {
            match &self.keychain {
                Some(k) => format!(" \"{}\"", k.display()),
                None => String::new(),
            }
        }
        fn cmd(&self) -> Command {
            let mut c = Command::new("security");
            if let Some(p) = &self.path {
                c.env("PATH", p);
            }
            c
        }
        pub fn get(&self, host: &str) -> Result<Option<Token>, Error> {
            let out = self
                .cmd()
                .args(["find-generic-password", "-s", "vcs", "-a", host, "-w"])
                .args(self.keychain.iter())
                .output()
                .map_err(|e| Error::Msg(format!("cannot run `security`: {e}")))?;
            match out.status.code() {
                Some(0) => Ok(Some(Token::new(&String::from_utf8_lossy(&out.stdout))?)),
                Some(44) => Ok(None), // errSecItemNotFound
                _ => msg(format!("`security` failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
            }
        }
        /// The token goes to `security -i` on stdin, never into argv (visible in `ps`).
        pub fn store(&self, host: &str, t: &Token) -> Result<(), Error> {
            if t.expose().contains(['"', '\\', '\'']) {
                return msg("this token has characters the keychain helper cannot store; use credential.helper = file");
            }
            let line = format!(
                "add-generic-password -U -s vcs -a \"{host}\" -w \"{}\"{}\n",
                t.expose(),
                self.kc_arg()
            );
            let mut child = self
                .cmd()
                .arg("-i")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| Error::Msg(format!("cannot run `security`: {e}")))?;
            child
                .stdin
                .take()
                .expect("piped")
                .write_all(line.as_bytes())
                .map_err(|e| Error::Msg(format!("`security`: {e}")))?;
            let out = child.wait_with_output().map_err(|e| Error::Msg(format!("`security`: {e}")))?;
            if !out.status.success() {
                return msg(format!(
                    "`security` failed: {}",
                    t.scrub(String::from_utf8_lossy(&out.stderr).trim())
                ));
            }
            Ok(())
        }
        pub fn erase(&self, host: &str) -> Result<(), Error> {
            let out = self
                .cmd()
                .args(["delete-generic-password", "-s", "vcs", "-a", host])
                .args(self.keychain.iter())
                .output()
                .map_err(|e| Error::Msg(format!("cannot run `security`: {e}")))?;
            match out.status.code() {
                Some(0) | Some(44) => Ok(()),
                _ => msg(format!("`security` failed: {}", String::from_utf8_lossy(&out.stderr).trim())),
            }
        }
    }
}

// ---- resolution and commands ----

pub fn describe_source(host: &str, cfg: &Config) -> Result<(String, Option<Token>), Error> {
    if let Some(t) = std::env::var("VCS_TOKEN").ok().filter(|t| !t.trim().is_empty()) {
        return Ok(("VCS_TOKEN environment variable".into(), Some(Token::new(&t)?)));
    }
    let h = configured_helper(cfg)?;
    let mut input = Kv::new();
    input.insert("host".into(), host.to_owned());
    let t = run_helper(&h, Op::Get, &input)?;
    Ok((format!("credential helper '{}'", h.label()), t))
}

/// The token for an `http://` remote: `VCS_TOKEN`, else the credential helper. `None` = not logged in.
pub fn token_for(url: &str) -> Result<Option<Token>, Error> {
    let host = host_key(url)?;
    let cfg = Config::load()?;
    Ok(describe_source(&host, &cfg)?.1)
}

fn read_token(from_stdin: bool) -> Result<Token, Error> {
    use std::io::IsTerminal;
    let stdin = std::io::stdin();
    let mut line = String::new();
    if from_stdin || !stdin.is_terminal() {
        stdin.lock().read_line_all(&mut line)?;
    } else {
        eprint!("Token: ");
        let _ = std::io::stderr().flush();
        let _ = Command::new("stty").arg("-echo").status();
        let r = stdin.lock().read_line_all(&mut line);
        let _ = Command::new("stty").arg("echo").status();
        eprintln!();
        r?;
    }
    Token::new(&line)
}

trait ReadLineAll {
    fn read_line_all(&mut self, out: &mut String) -> Result<(), Error>;
}
impl<R: std::io::BufRead> ReadLineAll for R {
    fn read_line_all(&mut self, out: &mut String) -> Result<(), Error> {
        self.read_line(out)
            .map(|_| ())
            .map_err(|e| Error::Msg(format!("cannot read token: {e}")))
    }
}

fn ask_whoami(url: &str, t: &Token) -> Result<String, Error> {
    let b = crate::remote::http_backend(url, t.expose()).map_err(|e| Error::Msg(t.scrub(&e.to_string())))?;
    b.whoami().map_err(|e| Error::Msg(t.scrub(&e.to_string())))
}

pub fn login(url: &str, token_stdin: bool) -> Result<(), Error> {
    let host = host_key(url)?;
    let cfg = Config::load()?;
    let helper = configured_helper(&cfg)?;
    let t = read_token(token_stdin)?;
    let who = ask_whoami(url, &t)?;
    let mut input = Kv::new();
    input.insert("host".into(), host.clone());
    input.insert("token".into(), t.expose().to_owned());
    run_helper(&helper, Op::Store, &input).map_err(|e| Error::Msg(t.scrub(&e.to_string())))?;
    println!(
        "logged in to {host} as {who} (token saved with credential helper '{}')",
        helper.label()
    );
    if std::env::var("VCS_TOKEN").is_ok_and(|v| !v.is_empty()) {
        println!("note: VCS_TOKEN is set and takes precedence over the saved token");
    }
    Ok(())
}

pub fn logout(url: &str) -> Result<(), Error> {
    let host = host_key(url)?;
    let helper = configured_helper(&Config::load()?)?;
    let mut input = Kv::new();
    input.insert("host".into(), host.clone());
    run_helper(&helper, Op::Erase, &input)?;
    println!("logged out of {host} (credential helper '{}')", helper.label());
    if std::env::var("VCS_TOKEN").is_ok_and(|v| !v.is_empty()) {
        println!("note: VCS_TOKEN is still set in the environment");
    }
    Ok(())
}

pub fn status(url: &str) -> Result<(), Error> {
    println!("remote: {url}");
    let host = match host_key(url) {
        Ok(h) => h,
        Err(_) => {
            println!("this remote does not use tokens");
            return Ok(());
        }
    };
    let cfg = Config::load()?;
    match describe_source(&host, &cfg)? {
        (src, Some(t)) => {
            println!("token: present (from {src})");
            match ask_whoami(url, &t) {
                Ok(w) => println!("server: authenticated as {w}"),
                Err(e) => println!("server: {e}"),
            }
        }
        (src, None) => println!("token: none in {src}\nrun `vcs login {url}` or set VCS_TOKEN"),
    }
    Ok(())
}

/// `vcs credential get|store|erase`: the helper protocol front end.
pub fn credential(op: Op) -> Result<(), Error> {
    let mut text = String::new();
    std::io::stdin()
        .read_to_string(&mut text)
        .map_err(|e| Error::Msg(format!("cannot read stdin: {e}")))?;
    let input = parse_kv(&text);
    let host = kv_host(&input)?;
    let helper = configured_helper(&Config::load()?)?;
    let scrub_err = |e: Error| match input.get("token").map(|t| Token::new(t)) {
        Some(Ok(t)) => Error::Msg(t.scrub(&e.to_string())),
        _ => e,
    };
    let got = run_helper(&helper, op, &input).map_err(scrub_err)?;
    if let Some(t) = got {
        let mut out = Kv::new();
        out.insert("host".into(), host);
        out.insert("token".into(), t.expose().to_owned());
        print!("{}", render_kv(&out));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn hosts_with_quotes_or_backslashes_are_refused() {
        // the host is interpolated into a `security -i` command line by the keychain helper
        for bad in ["http://a\"b/", "http://a'b:1/", "http://a\\b/", "http://x\" -w \"y/", "http://a=b/"] {
            assert!(host_key(bad).is_err(), "{bad}");
        }
        let mut kv = Kv::new();
        kv.insert("host".into(), "evil\"host".into());
        assert!(kv_host(&kv).is_err());
        assert_eq!(host_key("http://Good.Example:8480/x").unwrap(), "good.example:8480");
    }

    use super::*;

    #[test]
    fn token_never_prints() {
        let t = Token::new("sekrit-token").unwrap();
        assert!(!format!("{t:?} {t}").contains("sekrit"));
        assert_eq!(t.scrub("bad sekrit-token here"), "bad <redacted> here");
        assert!(Token::new("a b").is_err() && Token::new("  ").is_err());
    }

    #[test]
    fn hosts() {
        assert_eq!(host_key("http://Host:8480/x/y").unwrap(), "host:8480");
        assert_eq!(host_key("http://u:p@h:1").unwrap(), "h:1");
        assert!(host_key("/some/dir").is_err());
        assert!(host_key("s3://b").is_err());
    }

    #[test]
    fn kv_roundtrip() {
        let m = parse_kv("host=h:1\ntoken=abc=def\n\nignored=1\n");
        assert_eq!(m["token"], "abc=def");
        assert!(!m.contains_key("ignored"));
        assert_eq!(parse_kv(&render_kv(&m)), m);
    }

    #[test]
    fn helper_parsing_refuses_repo_commands() {
        assert_eq!(Helper::parse("file", Source::Repo).unwrap(), Helper::File);
        assert!(Helper::parse("!echo hi", Source::Repo).is_err());
        assert!(Helper::parse("/bin/x", Source::Repo).is_err());
        assert!(matches!(Helper::parse("!echo hi", Source::User).unwrap(), Helper::Shell(_)));
        assert!(Helper::parse("bogus", Source::User).is_err());
    }

    #[test]
    fn file_store_roundtrip_mode_and_refusal() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub").join("credentials");
        let t = Token::new("tok-1").unwrap();
        assert!(file_get(&p, "h:1").unwrap().is_none());
        file_store(&p, "h:1", &t).unwrap();
        file_store(&p, "h:2", &Token::new("tok-2").unwrap()).unwrap();
        assert_eq!(file_get(&p, "h:1").unwrap().unwrap().expose(), "tok-1");
        assert!(file_erase(&p, "h:1").unwrap());
        assert!(file_get(&p, "h:1").unwrap().is_none());
        assert_eq!(file_get(&p, "h:2").unwrap().unwrap().expose(), "tok-2");
        // no temp files left behind
        assert_eq!(std::fs::read_dir(p.parent().unwrap()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&p), 0o600);
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            let e = file_get(&p, "h:2").unwrap_err().to_string();
            assert!(e.contains("chmod 600") && !e.contains("tok-2"), "{e}");
            assert!(file_store(&p, "h:3", &t).is_err());
            // rewriting after the user fixes the mode keeps 0600
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
            file_store(&p, "h:3", &t).unwrap();
            assert_eq!(mode(&p), 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn keychain_with_fake_security_shim() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let store = d.path().join("store");
        std::fs::create_dir(&store).unwrap();
        let shim = d.path().join("security");
        let script = format!(
            r#"#!/bin/sh
S="{}"
case "$1" in
  find-generic-password)
    while [ $# -gt 0 ]; do [ "$1" = "-a" ] && A="$2"; shift; done
    [ -f "$S/$A" ] || {{ echo "could not be found" >&2; exit 44; }}
    cat "$S/$A"; exit 0;;
  -i)
    read -r CMD
    eval "set -- $CMD"
    while [ $# -gt 0 ]; do [ "$1" = "-a" ] && A="$2"; [ "$1" = "-w" ] && W="$2"; shift; done
    printf '%s\n' "$W" > "$S/$A"; exit 0;;
  delete-generic-password)
    while [ $# -gt 0 ]; do [ "$1" = "-a" ] && A="$2"; shift; done
    [ -f "$S/$A" ] || exit 44
    rm "$S/$A"; exit 0;;
esac
exit 1
"#,
            store.display()
        );
        std::fs::write(&shim, script).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::ffi::OsString::from(format!("{}:/usr/bin:/bin", d.path().display()));
        let k = keychain::Keychain::with_path(path);
        assert!(k.get("h:1").unwrap().is_none());
        k.store("h:1", &Token::new("kc-token").unwrap()).unwrap();
        assert_eq!(k.get("h:1").unwrap().unwrap().expose(), "kc-token");
        k.erase("h:1").unwrap();
        assert!(k.get("h:1").unwrap().is_none());
        k.erase("h:1").unwrap();
        assert!(k.store("h:1", &Token::new("bad\"q").unwrap()).is_err());
    }

    /// Opt-in test against the REAL `security` tool: `VCS_TEST_REAL_KEYCHAIN=1`. It creates a
    /// throwaway keychain file in a tempdir (never the login keychain), uses only that file, and
    /// deletes it afterwards.
    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_with_real_security_tool() {
        if std::env::var("VCS_TEST_REAL_KEYCHAIN").as_deref() != Ok("1") {
            eprintln!("skipped: set VCS_TEST_REAL_KEYCHAIN=1 to run against the real `security` tool");
            return;
        }
        let d = tempfile::tempdir().unwrap();
        let kc = d.path().join("vcs-test.keychain-db");
        let sec = |args: &[&str]| std::process::Command::new("security").args(args).output().unwrap();
        let kcs = kc.to_str().unwrap();
        assert!(sec(&["create-keychain", "-p", "pw", kcs]).status.success());
        // unlock and never auto-lock, so the test does not prompt
        assert!(sec(&["unlock-keychain", "-p", "pw", kcs]).status.success());
        let run = || -> Result<(), String> {
            let k = keychain::Keychain::with_keychain(kc.clone());
            assert!(k.get("h:1").unwrap().is_none());
            k.store("h:1", &Token::new("kc-token").unwrap()).unwrap();
            assert_eq!(k.get("h:1").unwrap().unwrap().expose(), "kc-token");
            k.store("h:1", &Token::new("kc-token2").unwrap()).unwrap(); // -U updates in place
            assert_eq!(k.get("h:1").unwrap().unwrap().expose(), "kc-token2");
            k.erase("h:1").unwrap();
            assert!(k.get("h:1").unwrap().is_none());
            k.erase("h:1").unwrap();
            Ok(())
        };
        let r = std::panic::catch_unwind(run);
        let _ = sec(&["delete-keychain", kcs]);
        if let Err(e) = r {
            std::panic::resume_unwind(e);
        }
    }
}
