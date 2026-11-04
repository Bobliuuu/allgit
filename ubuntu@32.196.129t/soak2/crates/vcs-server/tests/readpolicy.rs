//! Zone-level read policy (`allow-read` / `deny-read`): what a restricted token can and cannot read,
//! including requests crafted to get around the check. Residual leakage is asserted too, so the
//! documentation in docs/server.md stays honest (manifest and ids stay visible).

use std::collections::BTreeSet;
use std::sync::Arc;
use vcs_objects::*;
use vcs_remote::protocol::object_key;
use vcs_remote::*;
use vcs_server::{Config, HttpBackend, Policy, Principal, Role, Server, TokenTable};
use vcs_store::{MemoryStore, ObjectStore};

fn p(name: &str, role: Role) -> Principal {
    Principal {
        name: name.into(),
        role,
        zones: None::<BTreeSet<String>>,
    }
}

fn sig() -> Signature {
    Signature {
        name: "c".into(),
        email: "c".into(),
        timestamp: 1,
    }
}

struct Fx {
    mem: Arc<MemBackend>,
    srv: Server,
    url: String,
    art_commit: ObjectId,
    art_blob: ObjectId,
    code_commit: ObjectId,
    code_tree: ObjectId,
    code_blob: ObjectId,
    shared_blob: ObjectId,
}

fn put_all(mem: &MemBackend, s: &MemoryStore, ids: &[ObjectId]) {
    for id in ids {
        let o = s.get(id).unwrap();
        mem.put_if_absent(&object_key(id), &o.encode()).unwrap();
    }
}

fn zone_commit(s: &MemoryStore, zone: &str, files: &[(&str, &str)]) -> (ObjectId, ObjectId, Vec<ObjectId>) {
    let mut blobs = vec![];
    let entries = files
        .iter()
        .map(|(n, c)| {
            let id = s.put_blob(c.as_bytes()).unwrap();
            blobs.push(id);
            TreeEntry {
                name: (*n).into(),
                mode: EntryMode::File,
                id,
            }
        })
        .collect();
    let t = s.put(&Object::Tree(Tree::new(entries).unwrap())).unwrap();
    let c = s
        .put(&Object::Commit(Commit {
            zone: zone.into(),
            parents: vec![],
            root: t,
            change_id: ChangeId::from_bytes([zone.len() as u8; 16]),
            author: sig(),
            message: zone.into(),
        }))
        .unwrap();
    (c, t, blobs)
}

fn setup(policy: &str) -> Fx {
    let s = MemoryStore::new();
    let mem = Arc::new(MemBackend::new());
    let (ac, at, ab) = zone_commit(&s, "art", &[("hero.png", "art-secret-bytes"), ("shared.txt", "same in both")]);
    let (cc, ct, cb) = zone_commit(&s, "code", &[("main.rs", "fn secret() {}"), ("shared.txt", "same in both")]);
    let mut all = vec![ac, at, cc, ct];
    all.extend(&ab);
    all.extend(&cb);
    all.sort();
    all.dedup();
    put_all(&mem, &s, &all);
    let mut m = Manifest::default();
    m.branches.insert(("art".into(), "main".into()), ac);
    m.branches.insert(("code".into(), "main".into()), cc);
    m.zones.insert("art".into(), "art".into());
    m.zones.insert("code".into(), "code".into());
    mem.put_if_absent("manifest", &m.encode()).unwrap();
    let mut cfg = Config::new(TokenTable::from_entries(vec![
        ("t-rita", p("rita", Role::Read)),
        ("t-alice", p("alice", Role::Write)),
        ("t-root", p("root", Role::Admin)),
    ]));
    cfg.validator.policy = Policy::parse(policy).unwrap();
    let srv = Server::start(mem.clone(), cfg, "127.0.0.1:0").unwrap();
    let url = format!("http://{}", srv.local_addr());
    Fx {
        mem,
        srv,
        url,
        art_commit: ac,
        art_blob: ab[0],
        code_commit: cc,
        code_tree: ct,
        code_blob: cb[0],
        shared_blob: ab[1],
    }
}

fn cl(f: &Fx, t: &str) -> HttpBackend {
    HttpBackend::new(&f.url, t).unwrap()
}

/// Raw request through ureq so that odd paths reach the server unmodified.
fn raw(f: &Fx, tok: &str, method: &str, path: &str) -> (u16, String) {
    let r = ureq::request(method, &format!("{}{path}", f.url)).set("Authorization", &format!("Bearer {tok}"));
    match r.call() {
        Ok(r) => (r.status(), r.into_string().unwrap_or_default()),
        Err(ureq::Error::Status(c, r)) => (c, r.into_string().unwrap_or_default()),
        Err(e) => panic!("{e}"),
    }
}

#[test]
fn restricted_token_reads_only_its_zones() {
    let f = setup("allow-read rita art **\n");
    let rita = cl(&f, "t-rita");
    // own zone: commit, blob; shared content is readable because the art zone holds it too
    assert!(rita.get(&object_key(&f.art_commit)).unwrap().is_some());
    assert_eq!(
        rita.get(&object_key(&f.art_blob)).unwrap().unwrap().data.len(),
        Object::Blob(b"art-secret-bytes".to_vec()).encode().len()
    );
    assert!(rita.get(&object_key(&f.shared_blob)).unwrap().is_some());
    assert!(rita.get("manifest").unwrap().is_some());
    // the other zone: get, exists, range all look like "not found"
    for id in [f.code_commit, f.code_tree, f.code_blob] {
        let k = object_key(&id);
        assert!(rita.get(&k).unwrap().is_none(), "{k}");
        assert!(!rita.exists(&k).unwrap(), "{k}");
        assert!(rita.get_range(&k, 0, 4).unwrap().is_none(), "{k}");
    }
    // other principals are unaffected
    for t in ["t-alice", "t-root"] {
        let c = cl(&f, t);
        assert!(c.get(&object_key(&f.code_blob)).unwrap().is_some());
    }
}

#[test]
fn deny_read_applies_to_everyone_but_admin() {
    let f = setup("deny-read * code **\n");
    assert!(cl(&f, "t-alice").get(&object_key(&f.code_commit)).unwrap().is_none());
    assert!(cl(&f, "t-alice").get(&object_key(&f.art_commit)).unwrap().is_some());
    assert!(cl(&f, "t-rita").get(&object_key(&f.code_tree)).unwrap().is_none());
    assert!(
        cl(&f, "t-root").get(&object_key(&f.code_commit)).unwrap().is_some(),
        "admins are exempt"
    );
}

#[test]
fn adversarial_requests_do_not_get_around_the_check() {
    let f = setup("allow-read rita art **\n");
    let hex = f.code_blob.to_hex();
    let (a, b) = (&hex[..2], &hex[2..]);
    let ok_status = raw(
        &f,
        "t-rita",
        "GET",
        &format!("/v1/o/objects/{}/{}", &f.art_blob.to_hex()[..2], &f.art_blob.to_hex()[2..]),
    )
    .0;
    assert_eq!(ok_status, 200);
    let denied: Vec<String> = vec![
        format!("/v1/o/objects/{a}/{b}"),
        format!("/v1/o/objects%2F{a}%2F{b}"),                               // percent-encoded separators
        format!("/v1/o/objects/{}/{}", a.to_uppercase(), b.to_uppercase()), // other spelling of the same id
        format!("/v1/o/objects/{a}/{b}?offset=0&len=3"),                    // range
        format!("/v1/o/objects/{a}/{b}?offset=0"),                          // half a range falls back to full read
        format!("/v1/o/objects/{a}/{b}/"),                                  // trailing slash
        format!("/v1/o/objects//{a}/{b}"),
        format!("/v1/o/objects/./{a}/{b}"),
        format!("/v1/o/objects/x/../{a}/{b}"),
        "/v1/o/packs/anything.pack".to_string(),
        format!("/v1/o/tx/{hex}"),
        format!("/v1/o/tomb/{hex}"),
    ];
    for path in &denied {
        let (code, body) = raw(&f, "t-rita", "GET", path);
        assert!(code == 404 || code == 400, "GET {path} -> {code} {body}");
        assert!(!body.contains("secret"), "{path} leaked: {body}");
        let (code, _) = raw(&f, "t-rita", "HEAD", path);
        assert!(code != 200, "HEAD {path} -> {code}");
    }
    // the full body of a denied object never arrives, whatever the method
    for m in ["PUT", "DELETE"] {
        let (code, _) = raw(&f, "t-rita", m, &format!("/v1/o/objects/{a}/{b}"));
        assert!(code == 403 || code == 428, "{m} -> {code}");
    }
    // listing: refused for every prefix that could enumerate ids; packs look empty
    for prefix in ["", "o", "objects/", &format!("objects/{a}/"), "tx/", "tomb/", "gc/"] {
        let (code, body) = raw(&f, "t-rita", "GET", &format!("/v1/list?prefix={prefix}"));
        assert_eq!(code, 403, "prefix {prefix:?}: {body}");
        assert!(!body.contains(&hex[2..]));
    }
    assert_eq!(raw(&f, "t-rita", "GET", "/v1/list?prefix=packs/"), (200, String::new()));
    // an unrestricted token can still list
    assert!(cl(&f, "t-alice").list("objects/").unwrap().len() >= 6);
}

#[test]
fn new_history_and_policy_reload() {
    let f = setup("allow-read rita art **\n");
    let rita = cl(&f, "t-rita");
    assert!(rita.get(&object_key(&f.code_commit)).unwrap().is_none());
    // a new code commit published later is refused too (the index follows the manifest)
    let s = MemoryStore::new();
    let (nc, nt, nb) = zone_commit(&s, "code", &[("new.rs", "brand new secret")]);
    let mut ids = vec![nc, nt];
    ids.extend(nb.iter().copied());
    put_all(&f.mem, &s, &ids);
    let v = f.mem.get("manifest").unwrap().unwrap();
    let mut m = Manifest::decode(&v.data).unwrap();
    // unpublished objects are in no zone yet, so they are readable by id (documented)
    assert!(rita.get(&object_key(&nb[0])).unwrap().is_some());
    m.branches.insert(("code".into(), "main".into()), nc);
    f.mem.put_if_match("manifest", &m.encode(), &v.tag).unwrap().unwrap();
    for id in [nc, nt, nb[0]] {
        assert!(rita.get(&object_key(&id)).unwrap().is_none(), "published denied object {id}");
    }
    // reload: widen the policy, then narrow it again
    f.srv.reload_policy(Policy::parse("").unwrap());
    assert!(rita.get(&object_key(&nc)).unwrap().is_some());
    f.srv.reload_policy(Policy::parse("deny-read rita code **\n").unwrap());
    assert!(rita.get(&object_key(&nc)).unwrap().is_none());
    assert!(rita.get(&object_key(&f.art_commit)).unwrap().is_some());
}

#[test]
fn residual_leakage_is_what_the_docs_say() {
    let f = setup("allow-read rita art **\n");
    let rita = cl(&f, "t-rita");
    // the manifest names every zone and head: ids of denied zones are visible, content is not
    let m = Manifest::decode(&rita.get("manifest").unwrap().unwrap().data).unwrap();
    assert_eq!(m.head("code", "main"), Some(f.code_commit));
    assert!(rita.get(&object_key(&f.code_commit)).unwrap().is_none());
}
