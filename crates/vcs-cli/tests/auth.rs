//! Client authentication end to end: real `vcs` binary against an in-process `vcs-server`.
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use vcs_remote::MemBackend;
use vcs_server::{Config, Principal, Role, Server, TokenTable};

const WRITER: &str = "SECRET-writer-token-8f3a";
const READER: &str = "SECRET-reader-token-77c1";
const BOGUS: &str = "SECRET-bogus-token-0e9d";

fn start() -> (Server, String) {
    let p = |n: &str, role| Principal {
        name: n.into(),
        role,
        zones: None::<BTreeSet<String>>,
    };
    let table = TokenTable::from_entries(vec![(WRITER, p("wendy", Role::Write)), (READER, p("rita", Role::Read))]);
    let srv = Server::start(Arc::new(MemBackend::new()), Config::new(table), "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    (srv, url)
}

struct Env {
    home: tempfile::TempDir,
}
impl Env {
    fn run(&self, dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vcs"));
        c.args(args)
            .current_dir(dir)
            .env_remove("VCS_TOKEN")
            .env_remove("VCS_CREDENTIAL_HELPER")
            .env("VCS_CONFIG_HOME", self.home.path())
            .env("VCS_AUTHOR_NAME", "Dev")
            .env("VCS_AUTHOR_EMAIL", "dev@example.com")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn().unwrap();
        if let Some(s) = stdin {
            child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
        } else {
            drop(child.stdin.take());
        }
        let o = child.wait_with_output().unwrap();
        for t in [WRITER, READER, BOGUS] {
            let all = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
            // `credential get` prints the token on purpose; everything else must not.
            if args.first() != Some(&"credential") {
                assert!(!all.contains(t), "token leaked by vcs {args:?}: {all}");
            }
        }
        o
    }
    fn ok(&self, dir: &Path, args: &[&str]) -> String {
        let o = self.run(dir, args, None, &[]);
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    }
    fn err(&self, dir: &Path, args: &[&str], stdin: Option<&str>, env: &[(&str, &str)]) -> String {
        let o = self.run(dir, args, stdin, env);
        assert!(!o.status.success(), "{args:?} should fail");
        String::from_utf8_lossy(&o.stderr).into_owned()
    }
}

fn env() -> Env {
    Env {
        home: tempfile::tempdir().unwrap(),
    }
}

#[test]
fn login_status_logout_and_precedence() {
    let (_s, url) = start();
    let e = env();
    let cwd = e.home.path().to_owned();

    // a bad token is rejected with advice and nothing is saved
    let m = e.err(&cwd, &["login", &url, "--token-stdin"], Some(&format!("{BOGUS}\n")), &[]);
    assert!(m.contains("401") && m.contains("vcs login"), "{m}");
    assert!(!e.home.path().join("credentials").exists());

    let out = e.run(&cwd, &["login", &url, "--token-stdin"], Some(&format!("{WRITER}\n")), &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stdout).contains("wendy"));
    let creds = e.home.path().join("credentials");
    assert!(std::fs::read_to_string(&creds).unwrap().contains(WRITER));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&creds).unwrap().permissions().mode() & 0o777, 0o600);
    }

    let st = e.ok(&cwd, &["auth", "status", &url]);
    assert!(st.contains("authenticated as wendy"), "{st}");

    // VCS_TOKEN beats the stored token
    let o = e.run(&cwd, &["auth", "status", &url], None, &[("VCS_TOKEN", READER)]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("rita"));

    // loose permissions are refused
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o644)).unwrap();
        let m = e.err(&cwd, &["auth", "status", &url], None, &[]);
        assert!(m.contains("chmod 600"), "{m}");
        std::fs::set_permissions(&creds, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    e.ok(&cwd, &["logout", &url]);
    let st = e.ok(&cwd, &["auth", "status", &url]);
    assert!(st.contains("token: none"), "{st}");
    assert!(!std::fs::read_to_string(&creds).unwrap().contains(WRITER));
}

#[test]
fn remote_commands_use_saved_token_and_report_403() {
    let (_s, url) = start();
    let e = env();
    let root = e.home.path().join("work");
    std::fs::create_dir(&root).unwrap();
    e.ok(&root, &["init", "a"]);
    let a = root.join("a");
    std::fs::write(a.join("f"), "1\n").unwrap();
    e.ok(&a, &["add", "."]);
    e.ok(&a, &["commit", "-m", "init"]);
    e.ok(&a, &["remote", "set", &url]);

    // not logged in
    let m = e.err(&a, &["publish"], None, &[]);
    assert!(m.contains("vcs login"), "{m}");

    // read-only token: 403 with advice
    e.run(&a, &["login", &url, "--token-stdin"], Some(READER), &[]);
    let m = e.err(&a, &["publish"], None, &[]);
    assert!(m.contains("403") && m.contains("vcs login"), "{m}");

    // wrong token via env: 401
    let m = e.err(&a, &["publish"], None, &[("VCS_TOKEN", BOGUS)]);
    assert!(m.contains("401"), "{m}");

    // writer token works, and the token is never written into the repository
    e.run(&a, &["login", &url, "--token-stdin"], Some(WRITER), &[]);
    let out = e.ok(&a, &["publish"]);
    assert!(out.contains("published atomically"), "{out}");
    for f in ["remote", "config"] {
        if let Ok(t) = std::fs::read_to_string(a.join(".vcs").join(f)) {
            assert!(!t.contains("SECRET"), "{f}: {t}");
        }
    }
    let rem = std::fs::read_to_string(a.join(".vcs/remote")).unwrap();
    assert!(rem.contains(&url));
}

#[test]
fn credential_protocol() {
    let e = env();
    let cwd = e.home.path().to_owned();
    let get = || {
        let o = e.run(&cwd, &["credential", "get"], Some("host=H.example:1\n\n"), &[]);
        assert!(o.status.success());
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    assert_eq!(get(), "");
    let o = e.run(
        &cwd,
        &["credential", "store"],
        Some(&format!("host=h.example:1\ntoken={WRITER}\n")),
        &[],
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(get(), format!("host=h.example:1\ntoken={WRITER}\n"));
    e.run(&cwd, &["credential", "erase"], Some("host=h.example:1\n"), &[]);
    assert_eq!(get(), "");

    // external helper through a user-level config
    let helper = e.home.path().join("helper.sh");
    std::fs::write(
        &helper,
        "#!/bin/sh\nif [ \"$1\" = get ]; then cat >/dev/null; echo token=ext-helper-token; fi\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    e.ok(&cwd, &["config", "set", "--user", "credential.helper", helper.to_str().unwrap()]);
    let o = e.run(&cwd, &["credential", "get"], Some("host=x:2\n"), &[]);
    assert!(String::from_utf8_lossy(&o.stdout).contains("token=ext-helper-token"));
}
