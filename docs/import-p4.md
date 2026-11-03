# Importing recorded Perforce output

Status: **tested once against a real server** (Helix Core `p4d` 2026.1, `p4` 2025.2, one small history, see
[docs/research/perforce-benchmark.md](research/perforce-benchmark.md)). The fixtures in
`crates/vcs-repo/tests/fixtures/p4real` are real `p4 -G describe -s`, `p4 changes -l` and `p4 print -q` output, and
the importer test compares the tree of every changelist with what `p4 sync //depot/...@N` wrote (paths, +x, symlinks,
sha256 of contents). They agree for add, edit, delete, move, branch, integrate, `+x`, `symlink`, `binary`, `+k`,
empty files, CRLF text, spaces and non-ASCII (UTF-8) in names. Two discrepancies were found, both fixed: depot paths with
`@ # % *` arrive percent-escaped (`%40` ...), and `utf16` files: `p4 print` returns UTF-8 text where `p4 sync` writes
UTF-16LE with a BOM (the importer converts back; assumes a non-unicode server and the default little-endian client). Everything
else listed under Limits is still untested against a real server.

The core never runs `p4`. You record the output wherever Perforce exists and hand the importer a directory:

```bash
p4 changes -l -s submitted //depot/proj/... > fx/changes.txt
p4 -G describe -s 101 > fx/describe/101.marshal        # or: p4 describe -s 101 > fx/describe/101.txt
p4 print -q //depot/proj/src/main.c@101 > fx/print/101/depot/proj/src/main.c   # one per added/edited file
vcs init mirror && cd mirror
vcs import-p4 ../fx --view ../proj.view [--branch main]
```

## Directory layout

| Path | Content |
|---|---|
| `changes.txt` | `p4 changes -l` text; optional (without it every `describe/<N>.*` is used). `*pending*` entries are ignored |
| `describe/<N>.marshal` | `p4 -G describe -s N`: Python-marshalled dictionaries (parser: `p4_marshal.rs`). Preferred: it has file types and epoch times |
| `describe/<N>.txt` | `p4 describe -s N` text. No file types (all `text`); date read as UTC |
| `print/<N>/<depot path without //>` | `p4 print -q` content of each added/edited file at that change |

## View file

One rule per line, `<depot> <zone path>`; later rules win; `#` comments. `//depot/proj/... app/...` maps a subtree,
`-//depot/proj/secret/...` excludes one, a line without `...` maps a single file. A right side `//client/app/...` has
`//client/` stripped. Only a trailing `...` wildcard is supported; `*`, `%%1` and quoted paths are refused.

## Mapping

* Each submitted changelist with at least one file inside the view becomes **one commit** in the root zone, parented on
  the previous one (changelists are applied in ascending number). A changelist with no file in the view makes no commit
  and is reported as not imported.
* Author = Perforce user (`user@perforce`), timestamp = submit time, message = description plus a trailer
  `Perforce-Change: <N>`. The workspace (client) name is dropped. `change_id` is derived from `p4:<N>:root`.
* Table `.vcs/p4-import` (`<commit id> p4:<N>:root`): re-import skips known changelists, so it is idempotent and an
  extended fixture directory imports only the new changes.
* Actions: `add edit branch integrate import move/add` write content; `delete move/delete purge archive` remove the
  file. `integrate`/`branch` are plain content changes (no second parent), a move is a delete plus an add (no rename
  tracking). Any other action is an error.
* File types: `+x` and the legacy `xtext`/`xbinary`... map to Executable; `symlink` to a Symlink (content = target, one
  trailing newline in the fixture is dropped). `apple`, `resource` and `+k` keyword-expanded files are
  imported as the bytes `p4 print` gave and reported as type warnings.

## Limits

* Real output was tested only on one small history on a non-unicode server (see the top). Not tested: real `p4 -G` output, unicode servers, large files (content is read
  whole into memory), obliterated or renumbered history, streams, labels (no snapshots are created), shelved or pending
  changes, `+l`/exclusive-open locks, `p4 print` of files whose type needs `-o` handling.
* Only the root zone and a single branch (`--branch`, default `main`) are written. If that branch already has history it
  must not be checked out, and must be an ancestor of the import.
* Perforce timestamps from the text form carry no time zone and are read as UTC.
