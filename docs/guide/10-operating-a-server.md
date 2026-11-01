# 10. Operating a server

`vcs-server` serves a directory or an S3 bucket over HTTP, with tokens, roles, an audit log and rate limits.
This chapter is a short tour. The full recipe is [../deploy.md](../deploy.md). Protocol and limits are in
[../server.md](../server.md). Security notes are in [../security.md](../security.md).

**By default the server speaks plain HTTP.** Tokens travel in clear text. Bind it to loopback or a private network.
Use `--tls-cert` and `--tls-key` with a `--features tls` build, or put a TLS proxy in front.
Never expose a plain HTTP port to the internet.

## Start it

You need a tokens file (one `<principal> <read|write|admin> <zones|*> <blake3-hex>` line each, see
[chapter 5](05-remotes.md#a-vcs-server-remote)) and a place to store data. Add an audit log with a key
to make it tamper-evident.

```text
$ vcs-server --help
vcs-server: usage: vcs-server (--root <dir> | --backend <s3://...|dir>) --tokens <file> [--listen host:port] [--audit-log <file> [--audit-key <file>]] [--rate-limit <burst>,<per-sec>] [--max-connections <n>] [--max-body <bytes>] [--policy <file>] [--max-lease-secs N] [--skew-epsilon-ms N] [--tls-cert <pem> --tls-key <pem>]
       vcs-server hash-token <token>
       vcs-server audit verify <file> [--key <file>]
[exit 2]
$ head -c 24 /dev/urandom | base64 | tr -d "=+/" > admin.token; vcs-server hash-token "$(cat admin.token)" | wc -c
65
$ curl -s http://127.0.0.1:8491/healthz
ok
$ curl -s -o /dev/null -w "%{http_code}\n" http://127.0.0.1:8491/v1/whoami
401
$ curl -s -H "Authorization: Bearer $(cat admin.token)" http://127.0.0.1:8491/v1/whoami
ada admin *
$ VCS_TOKEN=bob-token-xyz curl -s -H "Authorization: Bearer bob-token-xyz" http://127.0.0.1:8491/v1/whoami
bob write art
$ vcs init repo && cd repo && echo hi > a.txt && vcs add . && vcs commit -m hi && export VCS_TOKEN=$(cat ../admin.token) && vcs remote set http://127.0.0.1:8491 && vcs publish; cd ..
Initialized empty vcs repository in /home/ubuntu/guide-run/ch10/repo/.vcs
staged 1 file(s), 0 removal(s)
[root/main 497b99018f33] hi
remote set to http://127.0.0.1:8491
root/main (none) -> 497b99018f33
published atomically (remote generation 1)
```

`/healthz` needs no token. `/v1/whoami` needs one and tells you who you are.
Rotate a token by replacing its hash. The server re-reads the tokens file about once a second,
so removing a line revokes it without a restart.

The command line, for reference:

```text
vcs-server (--root <dir> | --backend <s3://...|dir>) --tokens <file> [--listen host:port]
           [--audit-log <file> [--audit-key <file>]] [--rate-limit <burst>,<per-sec>]
           [--max-connections <n>] [--max-body <bytes>] [--policy <file>]
           [--max-lease-secs N] [--skew-epsilon-ms N] [--tls-cert <pem> --tls-key <pem>]
vcs-server hash-token <token>
vcs-server audit verify <file> [--key <file>]
```

## Audit log

Every write is logged in a hash chain. `vcs-server audit verify` checks the chain.
Change one record and it says so.

```text
$ wc -l < audit.log
4
$ vcs-server audit verify audit.log --key audit.key
ok: 4 records, head 8c2efeebb9a5544f7e55d3b39b6ac921f7efb902e39190c4209e01eba635b33f
$ cut -c1-160 audit.log | head -3
{"seq":0,"ts_ms":1793500358513,"principal":"ada","op":"PUT","key":"packs/pack-744679da3efe017e9cdebf5e598ff5354aa55f9c169ca9a07591790e63990d99.pack","outcome":"
{"seq":1,"ts_ms":1793500358525,"principal":"ada","op":"PUT","key":"packs/pack-744679da3efe017e9cdebf5e598ff5354aa55f9c169ca9a07591790e63990d99.idx","outcome":"2
{"seq":2,"ts_ms":1793500358537,"principal":"ada","op":"PUT","key":"tx/32bc54f09fb235df15c2bed093e63e77a39251d58cd9bb39429b86ad48dec9ed","outcome":"201","prev":"
$ sed -i "2s/ada/eve/" audit.log; vcs-server audit verify audit.log --key audit.key
TAMPERED OR INVALID: line 2: record hash mismatch (record was modified)
[exit 1]
```

## Other things to know

- A path policy file (`--policy`, deny by default) limits which paths each principal may change. See `deploy/policy.example`.
- With S3 you can run several servers on one bucket. Set `--skew-epsilon-ms` for clock differences.
- Back up the data directory or the bucket. `vcs gc --remote` only works on a directory remote ([chapter 6](06-large-files.md)).
- A container image and a systemd unit are in the `deploy/` folder. *Not run for this guide: the container and the unit.*
- *Not run: TLS, `--policy`, S3 behind the server.*

Next: [chapter 11, troubleshooting](11-troubleshooting.md).
