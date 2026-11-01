//! `vcs config`: layered key/value settings.
//!
//! Precedence, highest first: environment, repository (`.vcs/config`), user
//! (`$VCS_CONFIG_HOME/config`, else `$XDG_CONFIG_HOME/vcs/config`, else `~/.config/vcs/config`).
//! File format: one `key = value` per line; `#` or `;` start a comment line; values may be
//! double-quoted (with `\"` and `\\` escapes). Malformed files give an error, never a panic.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use vcs_repo::Error;

/// Known keys: (name, environment override, help).
pub const KEYS: &[(&str, &str, &str)] = &[
    ("user.name", "VCS_AUTHOR_NAME", "author name for commits, tags and stashes"),
    ("user.email", "VCS_AUTHOR_EMAIL", "author email for commits, tags and stashes"),
    ("core.editor", "VCS_EDITOR", "editor for `vcs commit` without -m"),
    ("merge.structural", "VCS_MERGE_STRUCTURAL", "on/off: structural merge (default on)"),
    (
        "merge.renames",
        "VCS_MERGE_RENAMES",
        "on/off: detect renames in merge/rebase (default off)",
    ),
    ("log.default-format", "VCS_LOG_FORMAT", "full or oneline (default full)"),
    (
        "remote.default",
        "VCS_REMOTE_DEFAULT",
        "remote URL used when `vcs remote set` was not run",
    ),
    (
        "credential.helper",
        "VCS_CREDENTIAL_HELPER",
        "token store: file (default), keychain (macOS), !command or /path/to/helper",
    ),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    Repo,
    User,
}

fn msg<T>(s: impl Into<String>) -> Result<T, Error> {
    Err(Error::Msg(s.into()))
}

pub fn validate_key(key: &str) -> Result<(), Error> {
    if KEYS.iter().any(|k| k.0 == key) {
        Ok(())
    } else {
        let known: Vec<&str> = KEYS.iter().map(|k| k.0).collect();
        msg(format!("unknown config key '{key}'\n\nKnown keys: {}", known.join(", ")))
    }
}

pub fn validate_value(key: &str, value: &str) -> Result<(), Error> {
    if value.contains('\n') || value.contains('\r') {
        return msg("config values cannot contain newlines");
    }
    match key {
        "merge.renames" if !matches!(value, "on" | "off") => msg("merge.renames must be 'on' or 'off'"),
        "merge.structural" if !matches!(value, "on" | "off") => msg("merge.structural must be 'on' or 'off'"),
        "log.default-format" if !matches!(value, "full" | "oneline") => msg("log.default-format must be 'full' or 'oneline'"),
        "user.name" | "user.email" | "core.editor" | "remote.default" if value.trim().is_empty() => {
            msg(format!("{key} cannot be empty (use `vcs config unset {key}`)"))
        }
        _ => Ok(()),
    }
}

pub fn parse(text: &str, origin: &Path) -> Result<BTreeMap<String, String>, Error> {
    let mut m = BTreeMap::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let bad = |why: &str| -> Result<BTreeMap<String, String>, Error> { msg(format!("{}:{}: {why}: {raw:?}", origin.display(), i + 1)) };
        let Some((k, v)) = line.split_once('=') else {
            return bad("expected `key = value`");
        };
        let k = k.trim();
        if k.is_empty() || k.chars().any(|c| c.is_whitespace()) {
            return bad("invalid key");
        }
        let v = v.trim();
        let val = if let Some(rest) = v.strip_prefix('"') {
            let mut out = String::new();
            let mut chars = rest.chars();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => match chars.next() {
                        Some('"') => out.push('"'),
                        Some('\\') => out.push('\\'),
                        _ => return bad("bad escape in quoted value"),
                    },
                    c => out.push(c),
                }
            }
            if !closed || !chars.as_str().trim().is_empty() {
                return bad("unterminated or trailing text after quoted value");
            }
            out
        } else {
            v.to_owned()
        };
        m.insert(k.to_owned(), val);
    }
    Ok(m)
}

fn render(m: &BTreeMap<String, String>) -> String {
    let mut s = String::new();
    for (k, v) in m {
        let plain = !v.is_empty() && v == v.trim() && !v.starts_with('"') && !v.starts_with('#') && !v.starts_with(';');
        if plain {
            s.push_str(&format!("{k} = {v}\n"));
        } else {
            s.push_str(&format!("{k} = \"{}\"\n", v.replace('\\', "\\\\").replace('"', "\\\"")));
        }
    }
    s
}

fn read_file(p: &Path) -> Result<BTreeMap<String, String>, Error> {
    match std::fs::read(p) {
        Ok(b) => match String::from_utf8(b) {
            Ok(t) => parse(&t, p),
            Err(_) => msg(format!("{}: not valid UTF-8", p.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(source) => Err(Error::Io {
            path: p.to_owned(),
            source,
        }),
    }
}

pub fn user_path() -> Option<PathBuf> {
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if let Some(h) = env("VCS_CONFIG_HOME") {
        return Some(h.join("config"));
    }
    if let Some(x) = env("XDG_CONFIG_HOME") {
        return Some(x.join("vcs").join("config"));
    }
    env("HOME").map(|h| h.join(".config").join("vcs").join("config"))
}

/// `.vcs/config` of the repository containing the current directory, if any.
pub fn repo_path() -> Option<PathBuf> {
    let mut cur = std::env::current_dir().ok()?.canonicalize().ok()?;
    loop {
        if cur.join(".vcs").is_dir() {
            return Some(cur.join(".vcs").join("config"));
        }
        if !cur.pop() {
            return None;
        }
    }
}

fn scope_path(scope: Scope) -> Result<PathBuf, Error> {
    match scope {
        Scope::Repo => repo_path().ok_or(Error::NotARepo),
        Scope::User => {
            user_path().ok_or_else(|| Error::Msg("cannot locate the user config directory (set HOME or VCS_CONFIG_HOME)".into()))
        }
    }
}

/// Where a value came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Env,
    Repo,
    User,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::Repo => "repo",
            Source::User => "user",
        }
    }
}

pub struct Config {
    repo: BTreeMap<String, String>,
    user: BTreeMap<String, String>,
}

impl Config {
    pub fn load() -> Result<Config, Error> {
        let user = match user_path() {
            Some(p) => read_file(&p)?,
            None => BTreeMap::new(),
        };
        let repo = match repo_path() {
            Some(p) => read_file(&p)?,
            None => BTreeMap::new(),
        };
        Ok(Config { repo, user })
    }

    pub fn get_with_source(&self, key: &str) -> Option<(String, Source)> {
        if let Some((_, var, _)) = KEYS.iter().find(|k| k.0 == key) {
            if let Some(v) = std::env::var(var).ok().filter(|v| !v.is_empty()) {
                return Some((v, Source::Env));
            }
        }
        if let Some(v) = self.repo.get(key) {
            return Some((v.clone(), Source::Repo));
        }
        self.user.get(key).map(|v| (v.clone(), Source::User))
    }

    pub fn get(&self, key: &str) -> Option<String> {
        self.get_with_source(key).map(|x| x.0)
    }

    /// merge.structural, default on. An invalid value is an error.
    pub fn structural(&self) -> Result<bool, Error> {
        match self.get("merge.structural").as_deref() {
            None | Some("on") => Ok(true),
            Some("off") => Ok(false),
            Some(o) => msg(format!("merge.structural must be 'on' or 'off', got '{o}'")),
        }
    }

    /// merge.renames, default off. An invalid value is an error.
    pub fn renames(&self) -> Result<bool, Error> {
        match self.get("merge.renames").as_deref() {
            None | Some("off") => Ok(false),
            Some("on") => Ok(true),
            Some(o) => msg(format!("merge.renames must be 'on' or 'off', got '{o}'")),
        }
    }

    pub fn oneline_log(&self) -> Result<bool, Error> {
        match self.get("log.default-format").as_deref() {
            None | Some("full") => Ok(false),
            Some("oneline") => Ok(true),
            Some(o) => msg(format!("log.default-format must be 'full' or 'oneline', got '{o}'")),
        }
    }

    /// Editor: core.editor, then $VISUAL, then $EDITOR.
    pub fn editor(&self) -> Option<String> {
        self.get("core.editor").or_else(|| {
            ["VISUAL", "EDITOR"]
                .iter()
                .find_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()))
        })
    }

    /// (name, email) or a helpful error naming how to set them.
    pub fn identity(&self) -> Result<(String, String), Error> {
        match (self.get("user.name"), self.get("user.email")) {
            (Some(n), Some(e)) => Ok((n, e)),
            (n, e) => {
                let missing = match (n.is_none(), e.is_none()) {
                    (true, true) => "user.name and user.email are not set",
                    (true, false) => "user.name is not set",
                    _ => "user.email is not set",
                };
                msg(format!(
                    "{missing}\n\nTell vcs who you are:\n  vcs config set --user user.name \"Your Name\"\n  vcs config set --user user.email you@example.com\n(drop --user to set it for this repository only)"
                ))
            }
        }
    }
}

pub fn set(scope: Scope, key: &str, value: &str) -> Result<(), Error> {
    validate_key(key)?;
    validate_value(key, value)?;
    let p = scope_path(scope)?;
    let mut m = read_file(&p)?;
    m.insert(key.to_owned(), value.to_owned());
    write(&p, &m)
}

pub fn unset(scope: Scope, key: &str) -> Result<bool, Error> {
    validate_key(key)?;
    let p = scope_path(scope)?;
    let mut m = read_file(&p)?;
    let had = m.remove(key).is_some();
    if had {
        write(&p, &m)?;
    }
    Ok(had)
}

fn write(p: &Path, m: &BTreeMap<String, String>) -> Result<(), Error> {
    let io = |source| Error::Io {
        path: p.to_owned(),
        source,
    };
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(io)?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, render(m)).map_err(io)?;
    std::fs::rename(&tmp, p).map_err(io)
}

/// All effective entries (key, value, source), for `vcs config list`.
pub fn list(cfg: &Config) -> Vec<(String, String, Source)> {
    let mut keys: Vec<String> = KEYS.iter().map(|k| k.0.to_owned()).collect();
    for k in cfg.repo.keys().chain(cfg.user.keys()) {
        if !keys.contains(k) {
            keys.push(k.clone());
        }
    }
    keys.sort();
    keys.into_iter()
        .filter_map(|k| cfg.get_with_source(&k).map(|(v, s)| (k, v, s)))
        .collect()
}

/// Run the editor on a message template and return the cleaned message.
pub fn edit_message(cfg: &Config, dir: &Path) -> Result<String, Error> {
    let Some(editor) = cfg.editor() else {
        return msg("no editor configured\n\nUse `vcs commit -m <message>`, or set one:\n  vcs config set --user core.editor <command>\n(or set $EDITOR)");
    };
    let file = dir.join("COMMIT_EDITMSG");
    let io = |source| Error::Io {
        path: file.clone(),
        source,
    };
    std::fs::write(
        &file,
        "\n# Write the commit message above. Lines starting with '#' are ignored.\n# An empty message aborts the commit.\n",
    )
    .map_err(io)?;
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{editor} \"$@\""))
        .arg("vcs-editor")
        .arg(&file)
        .status()
        .map_err(|e| Error::Msg(format!("could not start editor '{editor}': {e}")))?;
    if !status.success() {
        return msg(format!("editor '{editor}' exited with {status}; commit aborted"));
    }
    let text = match String::from_utf8(std::fs::read(&file).map_err(io)?) {
        Ok(t) => t,
        Err(_) => return msg("editor left a message that is not valid UTF-8"),
    };
    let _ = std::fs::remove_file(&file);
    let kept: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    let m = kept.join("\n").trim().to_owned();
    if m.is_empty() {
        return msg("aborting commit due to empty message");
    }
    Ok(m)
}
