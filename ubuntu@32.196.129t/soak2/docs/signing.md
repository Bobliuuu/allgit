# Signing commits and tags

Ed25519 signatures on commits and annotated tags, verified against an allowed-signers list, plus a
server rule that refuses unsigned history on chosen branches. Implementation:
`crates/vcs-objects/src/signing.rs`.

## Usage

```text
vcs config set --user user.signingkey ~/.ssh/id_ed25519      # unencrypted OpenSSH ed25519 key
vcs config set --user signing.allowedSigners ~/.config/vcs/allowed_signers
vcs commit -m msg -S            # or -S/path/to/key; commit.sign=true signs every commit
vcs tag v1 -m "release" -s
vcs verify-commit [rev]         # exit 0 only for a good signature by an allowed signer
vcs verify-tag v1
vcs log --show-signature        # also: vcs show prints the verdict for a tag
```

Allowed signers file: one `principal ssh-ed25519 <base64 key>` per line (the first two fields of
an OpenSSH `.pub` file after the principal), `#` comments. `signing.revoked` names a file of
`ssh-ed25519 <base64 key>` lines. Verdicts: good, BAD signature, unknown key, REVOKED, no signature.

Keys: only unencrypted OpenSSH ed25519 private keys are read. Passphrase-protected keys and
ssh-agent are **not supported** (`-S` fails with an explicit message; nothing is committed).
Other key types (RSA, ECDSA) are not supported.

## What is signed, and where the signature lives

Payload: `"allgit-signature-v1\n"` followed by the canonical encoding (`kind || version || payload`,
docs/object-model.md) of the object **without** its signature. The kind byte is inside the encoding,
so a commit signature cannot be replayed as a tag signature. The signature text is
`ssh-ed25519 <base64 OpenSSH public key blob> <base64 64-byte signature>`.

* Tags: the signature goes in the existing `signature` field.
* Commits have no signature field. Adding one would change the commit encoding and so every id
  (a format bump, FORMAT_VERSION 4). Instead the signature is a trailer block at the end of the
  message (`-----BEGIN ALLGIT SIGNATURE-----` ... `-----END ALLGIT SIGNATURE-----`). The signed
  message is the message cut at that block. `log` and `show` hide it. **No format change; ids of
  unsigned objects are unchanged.** The signature is part of the object, so the commit id covers it
  (signing changes the id, as in Git). A later format version may promote it to a real field.

Honesty note: the rebase/cherry-pick/merge/amend paths create new commits and do not re-sign them;
a rewritten commit is unsigned (or keeps a trailer that no longer verifies, because parents or root
changed, so it reports BAD).

## Server rule

In the vcs-server policy file: `require-signed <zone|*> <branch-glob>`. Start the server with
`--allowed-signers <file>` and optionally `--revoked-signers <file>` (read at start; not reloaded
by SIGHUP). When a manifest write moves a matching branch head, the validator that runs before the
manifest compare-and-swap walks the commits reachable from the new head and not from the old head
and refuses (403) unless each is validly signed by an allowed, unrevoked key. It applies to admins
too. Tags are not checked server-side. At most 100,000 commits are walked per side per update.

## Threat model

Proves: the holder of an allowed private key produced exactly this commit/tag content (tree, parents,
author, message, zone, change id) at some time. Anyone verifying with the same allowed-signers list
gets the same answer, offline.

Does not prove:

* **Key compromise.** A stolen key signs anything. Revocation (`signing.revoked`, `--revoked-signers`)
  marks a key bad for every signature by it, past and future; there is no signing-time check, so
  revoking invalidates legitimate old history too. Signed timestamps are not trusted (the author date
  is the committer's claim, covered by the signature but not independently attested).
* **Who the signer is beyond the list.** The principal name comes from your allowed-signers file;
  the signature does not bind it to the author name/email in the commit.
* **Freshness and rewrites.** A signature covers one commit, not a branch. An attacker with write
  access can move a branch back to an older signed commit (rollback) or drop signed commits;
  `require-signed` only checks new commits, not that history was preserved. A force-moved branch to
  a commit that was already on the remote passes. Combine with branch protection (no non-fast-forward).
* **Replay across zones/repositories.** The zone id is signed, but the same signed commit can be
  published to another remote or branch of the same zone.
* **Content outside the commit.** Blobs and trees are covered through the root id; refs, locks and
  the operation log are not signed.
* **Server trust.** The server enforces the rule; a malicious server operator can ignore it. Clients
  that need the guarantee run `verify-commit` themselves.
* **Algorithm.** Ed25519 (verify_strict, so malleable signatures are rejected). No post-quantum
  protection.
