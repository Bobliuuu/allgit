//! SIGHUP on the real `vcs-server` binary reloads the policy and the tokens (unix only).
#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};
use vcs_objects::*;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_server::{auth::hash_token, HttpBackend};
use vcs_store::{MemoryStore, ObjectStore};

struct Srv {
    child: Child,
    url: String,
    _err: BufReader<std::process::ChildStderr>,
}
impl Drop for Srv {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn hup(s: &Srv) {
    assert!(Command::new("kill")
        .args(["-HUP", &s.child.id().to_string()])
        .status()
        .unwrap()
        .success());
}
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(5) {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

#[test]
fn sighup_reloads_policy_and_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let b = FsBackend::open(&data).unwrap();
    // two zones, one commit each
    let s = MemoryStore::new();
    let mut m = Manifest::default();
    let mut heads = vec![];
    for zone in ["art", "code"] {
        let blob = s.put_blob(format!("{zone} secret").as_bytes()).unwrap();
        let t = s
            .put(&Object::Tree(
                Tree::new(vec![TreeEntry {
                    name: "f".into(),
                    mode: EntryMode::File,
                    id: blob,
                }])
                .unwrap(),
            ))
            .unwrap();
        let c = s
            .put(&Object::Commit(Commit {
                zone: zone.into(),
                parents: vec![],
                root: t,
                change_id: ChangeId::from_bytes([zone.len() as u8; 16]),
                author: Signature {
                    name: "c".into(),
                    email: "c".into(),
                    timestamp: 1,
                },
                message: zone.into(),
            }))
            .unwrap();
        for id in [blob, t, c] {
            b.put_if_absent(&object_key(&id), &s.get(&id).unwrap().encode()).unwrap();
        }
        m.branches.insert((zone.into(), "main".into()), c);
        m.zones.insert(zone.into(), zone.into());
        heads.push(c);
    }
    b.put_if_absent("manifest", &m.encode()).unwrap();

    let tokens = dir.path().join("tokens");
    let policy = dir.path().join("policy");
    std::fs::write(
        &tokens,
        format!("rita read * {}\nroot admin * {}\n", hash_token("t-rita"), hash_token("t-root")),
    )
    .unwrap();
    std::fs::write(&policy, "allow-read rita art **\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vcs-server"))
        .args([
            "--root",
            data.to_str().unwrap(),
            "--tokens",
            tokens.to_str().unwrap(),
            "--policy",
            policy.to_str().unwrap(),
        ])
        .args(["--listen", "127.0.0.1:0"])
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut line = String::new();
    let mut rd = BufReader::new(child.stderr.take().unwrap());
    rd.read_line(&mut line).unwrap();
    let url = line.split_whitespace().find(|w| w.starts_with("http://")).unwrap().to_owned();
    let srv = Srv { child, url, _err: rd };

    let rita = || HttpBackend::new(&srv.url, "t-rita").unwrap();
    let code = object_key(&heads[1]);
    let art = object_key(&heads[0]);
    assert!(rita().get(&art).unwrap().is_some());
    assert!(rita().get(&code).unwrap().is_none(), "denied before the reload");

    // broken policy + SIGHUP: the old policy stays in force
    std::fs::write(&policy, "this is not a policy\n").unwrap();
    hup(&srv);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        rita().get(&code).unwrap().is_none(),
        "a broken policy file must not open anything up"
    );

    // valid new policy + SIGHUP: rita may now read both zones
    std::fs::write(&policy, "# no read rules\n").unwrap();
    hup(&srv);
    until("policy reload", || rita().get(&code).unwrap().is_some());

    // and back: narrowing takes effect too
    std::fs::write(&policy, "deny-read rita code **\n").unwrap();
    hup(&srv);
    until("policy narrowing", || rita().get(&code).unwrap().is_none());

    // tokens: remove rita, SIGHUP, 401 (the 1 s file poll would get there too; the signal makes it immediate)
    std::fs::write(&tokens, format!("root admin * {}\n", hash_token("t-root"))).unwrap();
    hup(&srv);
    until("token reload", || rita().get(&art).is_err());
    assert!(HttpBackend::new(&srv.url, "t-root").unwrap().get(&code).unwrap().is_some());
}
