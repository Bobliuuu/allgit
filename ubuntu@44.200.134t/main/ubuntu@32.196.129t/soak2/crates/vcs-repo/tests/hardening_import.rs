//! Hardening of the fast-export parser (issue #88). The stream comes from outside the trust
//! boundary: import must return an error or a consistent repository, never panic, never
//! write an object the strict decoder would later reject, never buffer without bound.
use std::io::Read;
use vcs_repo::Repo;

const SEED: &str = "\
feature done
blob
mark :1
original-oid aaaa000000000000000000000000000000000001
data 6
hello
commit refs/heads/main
mark :2
original-oid bbbb000000000000000000000000000000000001
author Ann <ann@x> 1000 +0000
committer Ann <ann@x> 1000 +0000
data 8
initial
M 100644 :1 hello.txt
M 100644 inline \"q\\\"t/a.txt\"
data 3
abc

commit refs/heads/side
mark :3
committer Cy <cy@x> 3000 +0000
data 5
side
from :2
R hello.txt moved/hello.txt
C moved/hello.txt copy.txt
D q
deleteall
M 100755 :1 run.sh

commit refs/heads/main
mark :4
committer Ann <ann@x> 4000 +0000
data <<EOT
delimited
EOT
from :2
merge :3
N inline :1
data 1
n

reset refs/heads/x
from :4

tag v1
from :4
tagger T <t@x> 5000 +0000
data 2
t
done
";

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, b: &mut Vec<u8>) {
    const TOKENS: &[&[u8]] = &[
        b"\n",
        b" ",
        b"\"",
        b"\\",
        b"<",
        b">",
        b"data ",
        b"data <<",
        b":",
        b"M ",
        b"R ",
        b"C ",
        b"from ",
        b"merge ",
        b"999999999999999999999",
        b"0",
        b"\\777",
        b"inline",
        b"commit ",
        b"blob\n",
        b"refs/heads/",
        b"\0",
        b"\xff",
    ];
    for _ in 0..1 + rng.below(4) {
        match rng.below(6) {
            0 if !b.is_empty() => {
                let i = rng.below(b.len());
                b[i] ^= 1 << rng.below(8);
            }
            1 if !b.is_empty() => {
                let i = rng.below(b.len());
                b.remove(i);
            }
            2 => {
                let i = rng.below(b.len() + 1);
                b.insert(i, rng.next() as u8);
            }
            3 => {
                let n = rng.below(b.len() + 1);
                b.truncate(n);
            }
            _ => {
                let i = rng.below(b.len() + 1);
                let t = TOKENS[rng.below(TOKENS.len())];
                b.splice(i..i, t.iter().copied());
            }
        }
    }
}

fn repo() -> (tempfile::TempDir, Repo) {
    let d = tempfile::tempdir().unwrap();
    let r = Repo::init(&d.path().join("r")).unwrap();
    (d, r)
}

#[test]
fn identity_with_closing_bracket_before_opening_does_not_panic() {
    // Regression: `rest[lt + 1..gt]` panicked when the last '>' came before the first '<'.
    for ident in ["a> <b 1 +0000", "> < 1 +0000", "x>y<z 1 +0000", "<"] {
        let (_d, r) = repo();
        let s = format!("commit refs/heads/main\ncommitter {ident}\ndata 1\nm\n");
        assert!(r.import_git(s.as_bytes()).is_err(), "{ident:?}");
    }
}

#[test]
fn endless_line_is_cut_off() {
    // Regression: `read_until` buffered a newline-free stream without limit.
    struct Endless;
    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            buf.fill(b'a');
            Ok(buf.len())
        }
    }
    let (_d, r) = repo();
    let e = r.import_git(std::io::BufReader::new(Endless)).unwrap_err();
    assert!(e.to_string().contains("too long"), "{e}");
}

#[test]
fn huge_declared_data_length_is_refused_before_reading() {
    let (_d, r) = repo();
    let e = r.import_git(&b"blob\ndata 99999999999999\nx"[..]).unwrap_err();
    assert!(e.to_string().contains("too large"), "{e}");
    // a plausible length with a truncated stream still fails cleanly
    assert!(r.import_git(&b"blob\ndata 1000000\nx"[..]).is_err());
}

#[test]
fn commit_the_decoder_would_reject_is_not_stored() {
    // Regression: an oversized message (> 1 MiB) or identity (> 256 bytes) was written, and then
    // every later read of that commit failed the strict decode.
    let (_d, r) = repo();
    let big = "m".repeat((1 << 20) + 1);
    let s = format!("commit refs/heads/main\ncommitter A <a@x> 1 +0000\ndata {}\n{big}\n", big.len());
    assert!(r.import_git(s.as_bytes()).is_err());
    let name = "n".repeat(300);
    let s = format!("commit refs/heads/main\ncommitter {name} <a@x> 1 +0000\ndata 1\nm\n");
    assert!(r.import_git(s.as_bytes()).is_err());
    // 70 merge parents exceeds the 64-parent limit
    let mut s = String::from("blob\nmark :1\ndata 1\nx\ncommit refs/heads/b\nmark :2\ncommitter A <a@x> 1 +0000\ndata 1\nm\n\n");
    for i in 3..73 {
        s += &format!("commit refs/heads/b{i}\nmark :{i}\ncommitter A <a@x> 1 +0000\ndata 1\nm\nM 100644 :1 f{i}\n\n");
    }
    s += "commit refs/heads/b\nmark :100\ncommitter A <a@x> 1 +0000\ndata 1\nm\nfrom :2\n";
    for i in 3..73 {
        s += &format!("merge :{i}\n");
    }
    assert!(r.import_git(s.as_bytes()).is_err());
    assert!(r.fsck().unwrap().problems.is_empty(), "nothing undecodable may reach the store");
}

#[test]
fn seed_stream_imports_cleanly() {
    let (_d, r) = repo();
    let rep = r.import_git(SEED.as_bytes()).unwrap();
    assert!(rep.commits_imported >= 3);
    assert!(r.fsck().unwrap().problems.is_empty());
}

#[test]
fn mutation_loop_never_panics_and_leaves_a_sound_repository() {
    let mut rng = Rng(0xfeed_face_0bad_f00d);
    let (mut ok, mut err) = (0u32, 0u32);
    for _ in 0..300 {
        let mut b = SEED.as_bytes().to_vec();
        mutate(&mut rng, &mut b);
        let (_d, r) = repo();
        match r.import_git(&b[..]) {
            Ok(_) => ok += 1,
            Err(_) => err += 1,
        }
        let rep = r.fsck().unwrap();
        assert!(rep.problems.is_empty(), "fsck after hostile import: {:?}", rep.problems);
    }
    assert!(
        ok > 0 && err > 0,
        "mutations should hit both accepted ({ok}) and rejected ({err}) streams"
    );
}
