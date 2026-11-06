# Security policy

vcs is early research software with no stable release and no on-disk format promise. Treat it accordingly.

## Reporting a vulnerability

Please do not put the details of a vulnerability in a public issue. Open an issue titled "security contact request"
without details (or write to the address on the maintainer's GitHub profile) and a private channel will be arranged.
Include the version or commit, a description, and a reproducer when you have one. You can expect an acknowledgement
within a few days. There is no bounty.

## Scope

The trust boundaries, the limits the decoders enforce and the threat review live in [docs/security.md](docs/security.md)
and [docs/security-review.md](docs/security-review.md). In scope: memory-safety or panic bugs on hostile input to a
decoder, path escapes on checkout, authorization bypasses in `vcs-server` (roles, zone tokens, the read policy), and
loss of an acknowledged write. Out of scope: anything that needs write access to the object store or the bucket and
only produces an error, and denial of service by an authenticated writer with the admin role.

## Supported versions

Only the current `main`.
