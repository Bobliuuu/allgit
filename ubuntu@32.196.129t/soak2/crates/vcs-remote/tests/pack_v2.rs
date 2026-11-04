//! A remote may hold v1 and v2 packs side by side; every read path must handle both.
use vcs_objects::{id_of_encoded, Object, ObjectId};
use vcs_remote::packfetch::{fetch_from_any_pack, fetch_from_pack, ReadAhead, RemoteSource};
use vcs_remote::{fetch_many, Backend, MemBackend};
use vcs_store::pack::{build_pack_fmt, PackFormat};

fn noise(seed: u64, n: usize) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

fn objs(tag: u64) -> Vec<(ObjectId, Vec<u8>)> {
    let mut v = Vec::new();
    for i in 0..20u64 {
        // alternate compressible and incompressible; a few trees/commits would need real structure,
        // blobs are enough for the codec paths
        let data = if i % 2 == 0 {
            format!("{tag}-{i}-").repeat(400).into_bytes()
        } else {
            noise(tag * 100 + i, 2000)
        };
        v.push(Object::Blob(data).encode());
    }
    v.into_iter().map(|e| (id_of_encoded(&e), e)).collect()
}

fn put(b: &MemBackend, name: &str, o: Vec<(ObjectId, Vec<u8>)>, fmt: PackFormat) {
    let built = build_pack_fmt(o, fmt);
    b.put_if_absent(&format!("packs/{name}.pack"), &built.pack).unwrap();
    b.put_if_absent(&format!("packs/{name}.idx"), &built.idx).unwrap();
}

type Objs = Vec<(ObjectId, Vec<u8>)>;

fn mixed() -> (MemBackend, Objs, Objs) {
    let b = MemBackend::new();
    let (a, c) = (objs(1), objs(2));
    put(&b, "a-v1", a.clone(), PackFormat::V1);
    put(&b, "b-v2", c.clone(), PackFormat::V2);
    (b, a, c)
}

#[test]
fn single_fetch_from_v1_and_v2_packs() {
    let (b, a, c) = mixed();
    for (id, enc) in a.iter().chain(&c) {
        assert_eq!(&fetch_from_any_pack(&b, id).unwrap().unwrap(), enc);
    }
    assert_eq!(&fetch_from_pack(&b, "packs/b-v2", &c[3].0).unwrap().unwrap(), &c[3].1);
    assert_eq!(fetch_from_pack(&b, "packs/b-v2", &a[3].0).unwrap(), None);
}

#[test]
fn fetch_many_across_mixed_packs() {
    let (b, a, c) = mixed();
    let want: Vec<_> = a.iter().step_by(3).chain(c.iter().step_by(2)).cloned().collect();
    let ids: Vec<_> = want.iter().map(|w| w.0).collect();
    let mut got = fetch_many(&b, &ids).unwrap();
    got.sort_by_key(|g| g.0);
    let mut want = want;
    want.sort_by_key(|w| w.0);
    assert_eq!(got, want);
}

#[test]
fn remote_source_all_read_ahead_modes() {
    for ahead in [ReadAhead::None, ReadAhead::Metadata, ReadAhead::Whole] {
        let (b, a, c) = mixed();
        let mut src = RemoteSource::new(&b, ahead);
        for (id, enc) in a.iter().chain(&c) {
            assert_eq!(&src.fetch(id).unwrap().unwrap(), enc, "{ahead:?}");
        }
    }
}

#[test]
fn v2_ranged_read_fetches_less_than_the_pack() {
    let b = MemBackend::new();
    let o = objs(3);
    let want = o[7].clone();
    put(&b, "x", o, PackFormat::V2);
    let before = b.bytes_read();
    assert_eq!(fetch_from_pack(&b, "packs/x", &want.0).unwrap().unwrap(), want.1);
    let used = b.bytes_read() - before;
    let pack_len = b.get("packs/x.pack").unwrap().unwrap().data.len() as u64;
    assert!(used < pack_len, "read {used} of {pack_len}");
}

#[test]
fn v2_trailer_and_corruption_checks() {
    let o = objs(4);
    let ids: Vec<_> = o.iter().map(|x| x.0).collect();
    let built = build_pack_fmt(o, PackFormat::V2);
    for (label, edit) in [
        (
            "cut",
            Box::new(|mut p: Vec<u8>| {
                p.truncate(p.len() - 5);
                p
            }) as Box<dyn Fn(Vec<u8>) -> Vec<u8>>,
        ),
        (
            "extended",
            Box::new(|mut p: Vec<u8>| {
                p.push(0);
                p
            }),
        ),
        (
            "altered",
            Box::new(|mut p: Vec<u8>| {
                let n = p.len();
                p[n - 1] ^= 1;
                p
            }),
        ),
    ] {
        let b = MemBackend::new();
        b.put_if_absent("packs/p.pack", &edit(built.pack.clone())).unwrap();
        b.put_if_absent("packs/p.idx", &built.idx).unwrap();
        assert!(fetch_many(&b, &ids).is_err(), "{label}");
        assert!(fetch_from_pack(&b, "packs/p", &ids[0]).is_err(), "{label}");
    }
    // flipped payload byte (inside an entry): decode or hash check fails
    let b = MemBackend::new();
    let mut bad = built.pack.clone();
    bad[20] ^= 0xFF;
    b.put_if_absent("packs/p.pack", &bad).unwrap();
    b.put_if_absent("packs/p.idx", &built.idx).unwrap();
    assert!(ids.iter().any(|id| fetch_from_pack(&b, "packs/p", id).is_err()));
    // a damaged index is refused outright
    let b = MemBackend::new();
    let mut badi = built.idx.clone();
    badi[100] ^= 1;
    b.put_if_absent("packs/p.pack", &built.pack).unwrap();
    b.put_if_absent("packs/p.idx", &badi).unwrap();
    assert!(fetch_from_pack(&b, "packs/p", &ids[0]).is_err());
}
