# 5. Remotes

A remote is where you share work. `vcs` has three kinds. You pick one with `vcs remote set`.

| Kind | URL looks like | Use it for |
| --- | --- | --- |
| directory | `../shared`, `/srv/repo`, `file:///srv/repo` | a shared disk, a USB stick, tests |
| S3 | `s3://bucket/prefix?endpoint=URL&region=R` | any S3-compatible object store |
| vcs-server | `http://host:port` (`https://` needs a `tls` build) | a team, with tokens and roles |

The commands are the same for all three:

- `vcs publish` uploads your checked-out branches in **one atomic transaction**. If it fails, nothing changed.
- `vcs fetch` downloads objects and updates remote-tracking branches. Your own branches stay put.
- `vcs sync` fetches, then fast-forwards local branches where that is safe.
- `vcs clone URL DIR` makes a new repository from a remote.

## A directory remote

```text
$ mkdir ../shared
$ vcs remote set ../shared
remote set to /home/ubuntu/guide-run/ch05/shared
$ vcs remote show
remote: /home/ubuntu/guide-run/ch05/shared
no remote-tracking refs yet (run `vcs fetch`)
$ vcs publish
assets/main (none) -> 6a18820b3760
engine/main (none) -> 395adb72449f
root/main (none) -> c6de1f291712
published atomically (remote generation 1)
$ ls ../shared
manifest
packs
tx
$ cd .. && vcs clone shared bob
Cloned /home/ubuntu/guide-run/ch05/shared into bob
downloaded zones: assets, engine, root
skipped zones (not subscribed): (none)
$ ls bob; vcs -C bob zone list
README.md
assets
engine
assets         assets               subscribed, on main; 1 branch(es)
engine         engine               subscribed, on main; 1 branch(es)
root           (rest of repo)       subscribed, on main; 1 branch(es)
$ vcs -C bob log --zone engine --oneline
395adb72449f first playable
$ cd ada
```

The remote holds a `manifest` (the branch heads), `packs` (the data) and `tx` (transaction records).
Do not edit it by hand.

## Fetch and sync

```text
$ echo "fn main() { run(); }" > engine/main.rs && vcs add . && vcs commit -m "engine runs"
staged 3 file(s), 0 removal(s)
[engine/main 76db1c2f09e4] engine runs
$ vcs publish
engine/main 395adb72449f -> 76db1c2f09e4
published atomically (remote generation 2)
$ vcs -C ../bob fetch
fetched assets/main @ 6a18820b3760
fetched engine/main @ 76db1c2f09e4
fetched root/main @ c6de1f291712
$ vcs -C ../bob status
zone assets (assets/): branch main @ 6a18820b3760
zone engine (engine/): branch main @ 395adb72449f
zone root: branch main @ c6de1f291712
clean
$ vcs -C ../bob sync
assets/main: up to date
engine/main: fast-forwarded 395adb72449f -> 76db1c2f09e4
root/main: up to date
$ vcs -C ../bob log --zone engine --oneline
76db1c2f09e4 engine runs
395adb72449f first playable
```

## Sparse clones

Add `--zone` to `clone` to download only some zones. `--stats` prints the requests and bytes.
Other zones are listed as "not subscribed". Add one later with `vcs zone add`, then `vcs fetch --zone`.

```text
$ cd .. && vcs clone shared art-only --zone assets --stats
requests: 14, downloaded: 2621 bytes, uploaded: 0 bytes
Cloned /home/ubuntu/guide-run/ch05/shared into art-only
downloaded zones: assets
skipped zones (not subscribed): engine, root
  add one later with `vcs zone add <name>` then `vcs fetch --zone <name>`
$ ls art-only
assets
$ vcs -C art-only status
zone assets (assets/): branch main @ 6a18820b3760
not subscribed (not downloaded/materialized, NOT deleted): engine, root
clean
$ vcs -C art-only zone list
assets         assets               subscribed, on main; 1 branch(es)
engine         engine               not subscribed; 0 branch(es)
root           (rest of repo)       not subscribed; 0 branch(es)
$ vcs -C art-only zone add engine && ls art-only
subscribed to zone engine
assets
$ vcs -C art-only fetch --zone engine && vcs -C art-only sync && ls art-only
fetched engine/main @ 76db1c2f09e4
assets/main: up to date
engine/main: fast-forwarded (none) -> 76db1c2f09e4
assets
engine
$ cd ada
```

## When two people publish

`publish` refuses to overwrite someone else's work. It tells you the remote moved.
Fetch, merge the remote branch, and publish again. If the changes touch different files the merge is automatic.

```text
$ export VCS_AUTHOR_NAME=Bob VCS_AUTHOR_EMAIL=bob@example.com
$ cd ../bob && echo "// bob" >> engine/main.rs && vcs add . && vcs commit -m "bob edit" && vcs publish
staged 3 file(s), 0 removal(s)
[engine/main e94f1909e58c] bob edit
engine/main 76db1c2f09e4 -> e94f1909e58c
published atomically (remote generation 3)
$ unset VCS_AUTHOR_NAME VCS_AUTHOR_EMAIL; cd ../ada
$ echo "pub fn util() {}" > engine/util.rs && vcs add . && vcs commit -m "ada adds util"
staged 4 file(s), 0 removal(s)
[engine/main 25caf51a5e6e] ada adds util
$ vcs publish
error: Cannot publish engine/main:

Remote branch advanced to e94f1909e58c.

Your local commit: 25caf51a5e6e
Remote commit:     e94f1909e58c

Options:
  vcs sync
[exit 1]
$ vcs sync
assets/main: up to date
engine/main: diverged; merge with `vcs merge --remote main --zone engine`
root/main: up to date
$ vcs fetch
fetched assets/main @ 6a18820b3760
fetched engine/main @ e94f1909e58c
fetched root/main @ c6de1f291712
$ vcs merge --remote main --zone engine
fetched missing object e380d924a90a
engine: merged, new commit 6f034f5e7c4d
$ vcs log --zone engine --oneline
6f034f5e7c4d Merge remote-tracking branch 'origin/main' into main
e94f1909e58c bob edit
25caf51a5e6e ada adds util
76db1c2f09e4 engine runs
395adb72449f first playable
$ vcs publish
engine/main e94f1909e58c -> 6f034f5e7c4d
published atomically (remote generation 4)
```

(`vcs sync` does not merge diverged branches. It tells you which command to run.)

## An S3 remote

Credentials come only from the environment: `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY`
(and `AWS_SESSION_TOKEN` if you use one). They are never put in the URL or stored in `.vcs`.
The bucket must already exist. `vcs` never creates it.
The store must support conditional writes (`If-None-Match`, `If-Match`). `vcs` checks this on connect.
Details and limits: [../s3.md](../s3.md).

This run used **moto**, a local S3 imitation, on the box. It is not AWS. *Not run: real AWS S3, `https://` endpoints.*

```text
$ export AWS_ACCESS_KEY_ID=testing AWS_SECRET_ACCESS_KEY=testing
$ vcs remote set 's3://studio-bucket/team/repo?endpoint=http://127.0.0.1:9511&region=us-east-1'
remote set to s3://studio-bucket/team/repo?endpoint=http://127.0.0.1:9511&region=us-east-1
$ vcs publish
assets/main (none) -> e4fead135bcc
root/main (none) -> 7baed80eaefa
published atomically (remote generation 1)
$ cd .. && vcs clone 's3://studio-bucket/team/repo?endpoint=http://127.0.0.1:9511' from-s3
Cloned s3://studio-bucket/team/repo?endpoint=http://127.0.0.1:9511 into from-s3
downloaded zones: assets, root
skipped zones (not subscribed): (none)
$ ls from-s3; cd ada
README.md
assets
$ unset AWS_ACCESS_KEY_ID; vcs publish; export AWS_ACCESS_KEY_ID=testing
error: missing S3 credentials: environment variable AWS_ACCESS_KEY_ID is not set

Options:
  export AWS_ACCESS_KEY_ID=... AWS_SECRET_ACCESS_KEY=...
```

## A vcs-server remote

`vcs-server` is a small HTTP server that sits in front of a directory or a bucket.
It adds tokens, roles (`read`, `write`, `admin`), per-zone scopes and an audit log.
Setting one up for a team is covered in [chapter 10](10-operating-a-server.md).
Here is the client side.

An admin makes a token and stores only its hash on the server. Each line of the tokens file is
`<principal> <role> <zones|*> <blake3-hex>`. Here Ada may write everywhere and Carol may only read.

```text
$ token=$(head -c 24 /dev/urandom | base64 | tr -d "=+/") && echo "token generated"
token generated
$ hash=$(vcs-server hash-token "$token") && echo "hash is ${#hash} hex chars"
hash is 64 hex chars
$ sed "s/ [0-9a-f]\{64\}$/ <blake3-hex>/" srv/tokens
ada write * <blake3-hex>
carol read * <blake3-hex>
$ curl -s http://127.0.0.1:8490/healthz
ok
$ vcs init team && cd team && echo "hello" > a.txt && vcs add . && vcs commit -m "hello"
Initialized empty vcs repository in /home/ubuntu/guide-run/ch05b/team/.vcs
staged 1 file(s), 0 removal(s)
[root/main 04ab5a460be8] hello
```

Log in once. The token is checked against the server and saved with the credential helper
(a file in `~/.config/vcs/credentials`, mode 0600, by default; `keychain` on macOS).
`VCS_TOKEN` overrides it. The token is never put in `.vcs/remote` or in an error message.

```text
$ vcs remote set http://127.0.0.1:8490
remote set to http://127.0.0.1:8490
$ vcs publish
error: not logged in to http://127.0.0.1:8490

Run `vcs login http://127.0.0.1:8490` (or set the VCS_TOKEN environment variable). Tokens are never part of the URL.
[exit 1]
$ echo "$token" | vcs login http://127.0.0.1:8490 --token-stdin
logged in to 127.0.0.1:8490 as ada write * (token saved with credential helper 'file')
$ vcs auth status
remote: http://127.0.0.1:8490
token: present (from credential helper 'file')
server: authenticated as ada write *
$ vcs publish
root/main (none) -> 04ab5a460be8
published atomically (remote generation 1)
$ cd .. && vcs clone http://127.0.0.1:8490 from-http --stats
requests: 11, downloaded: 918 bytes, uploaded: 0 bytes
Cloned http://127.0.0.1:8490 into from-http
downloaded zones: root
skipped zones (not subscribed): (none)
$ ls from-http; cd team
a.txt
$ VCS_TOKEN=carol-token-123 vcs auth status
remote: http://127.0.0.1:8490
token: present (from VCS_TOKEN environment variable)
server: authenticated as carol read *
$ echo x > b.txt; vcs add .; vcs commit -m "more"; VCS_TOKEN=carol-token-123 vcs publish
staged 2 file(s), 0 removal(s)
[root/main 06ab35a65c70] more
error: backend error: permission denied (403): this token's role or zone scope does not allow the operation (Forbidden: read-only principal). Ask the server admin for a token with the needed role/zones, then `vcs login http://127.0.0.1:8490`
[exit 1]
$ VCS_TOKEN=wrong-token vcs fetch
error: backend error: authentication failed (401): the server rejected the token (unauthorized); it may be missing, expired or revoked. Run `vcs login http://127.0.0.1:8490` to store a new one
[exit 1]
$ vcs logout
logged out of 127.0.0.1:8490 (credential helper 'file')
$ vcs auth status
remote: http://127.0.0.1:8490
token: none in credential helper 'file'
run `vcs login http://127.0.0.1:8490` or set VCS_TOKEN
```

Things to notice:

- `403` means your token's role or zone scope does not allow the call. `401` means the server rejected the token.
- `vcs logout` forgets the saved token. `vcs auth status` shows who the server thinks you are.
- Plain `http://` sends the token in clear text. Use loopback, a private network, or a TLS proxy.
  *Not run: a `tls` build and `https://`.*

Next: [chapter 6, large files](06-large-files.md).
