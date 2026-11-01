# 9. Structural and AI-assisted merge

Both features are **opt-in**. The default `vcs` does neither.

## Structural merge

A line merge cannot see that a function moved. The structural merge parses the file, matches whole items
(functions, methods, classes) by name, and merges those. It supports Rust, Python, TypeScript (`.ts`) and Go.
If it cannot be sure, it falls back to the normal line merge. If all three inputs parse, the result must parse too.

It needs a build with a feature flag, because it links tree-sitter:

```sh
cargo build --release -p vcs-cli --features structural                 # Rust
cargo build --release -p vcs-cli --features structural,structural-python,structural-typescript,structural-go
```

One branch moves `gamma` to the top of the file. The other changes the body of `gamma`.
First the default binary:

```text
$ cd plain && vcs merge mover
automatic merge stopped: 1 conflict(s)
  conflict: lib.rs

Resolve the files, `vcs add` each one, then `vcs commit`.
Inspect with `vcs conflict list|show <path>`; give up with `vcs merge --abort`.
error: merge has conflicts
[exit 1]
$ cat lib.rs
fn gamma() -> i32 {
    3
}

fn alpha() -> i32 {
    1
}

fn beta() -> i32 {
    2
<<<<<<< ours (root/editor)
}

fn gamma() -> i32 {
    33
||||||| base
}

fn gamma() -> i32 {
    3
=======
>>>>>>> mover
}
$ vcs merge --abort; cd ..
merge aborted; working tree restored
```

Now the same history with the binary built with `--features structural`
(shown as `vcs-structural`; on the box it was a separate build in its own target directory):

```text
$ cd structural && vcs-structural merge mover
root: merged, new commit f7570edd74f1
$ cat lib.rs
fn gamma() -> i32 {
    33
}

fn alpha() -> i32 {
    1
}

fn beta() -> i32 {
    2
}
$ vcs log --oneline -n 3; cd ..
f7570edd74f1 Merge branch 'mover' into editor
ee4c04f82022 gamma returns 33
2d8a54fb26ec move gamma to the top
```

`vcs merge --no-structural` turns it off for one merge. The two binaries print the same version, so the
only way to tell them apart is how they behave:

```text
$ vcs --version; vcs-structural --version
vcs 0.1.0
vcs 0.1.0
```

(Both paths shown are binaries from the same source; only the second was built with the feature.)
Measured gain so far is on a synthetic corpus only, and the whole gain is in moved code.
See [../merge.md](../merge.md). *Not run: Python, TypeScript and Go builds.*

## Assisted merge

An external command can **propose** a resolution for a text conflict. `vcs` never applies a proposal by itself.
You review it with `vcs conflict proposals` and apply it with `vcs resolve <path> --proposal`.

### Privacy warning

**The assistant gets your file contents.** With the ready-made `vcs-assist-claude`, the base, ours and theirs versions
of every conflicting file are sent in full over HTTPS to `api.anthropic.com`. It also costs money, billed to your key.
The model can be wrong. Do not use it on repositories you may not send to a third party,
and never on files that hold secrets. Details: [../assist.md](../assist.md).

By default no assistant runs. You consent in one of two ways:

- pass `--assist <command>` to one `vcs merge` or `vcs rebase`, or
- set `merge.assist` yourself.

`--no-assist` turns it off for one command, and `merge.assist off` turns it off for good.

```text
$ env -u ANTHROPIC_API_KEY vcs-assist-claude < req.json
vcs-assist-claude: ANTHROPIC_API_KEY is not set
[exit 1]
$ vcs config set merge.assist vcs-assist-claude
$ vcs config list
merge.assist=vcs-assist-claude  (repo)
user.email=ada@example.com  (user)
user.name=Ada Lovelace  (user)
$ vcs config set merge.assist off
$ vcs config unset merge.assist
```

The run above shows the opt-in setting and that `vcs-assist-claude` refuses to start without `ANTHROPIC_API_KEY`.
**Not run: a live call to the Anthropic API.** No key was used and no file left the box.

### The flow, with a stand-in

To show the flow without sending anything anywhere, this run uses a shell script as the assistant.
The script is **not a model**. It ignores its input and returns a fixed answer.
The protocol is: the request is JSON on stdin, the answer is `{"resolution": "...", "explanation": "..."}` on stdout.

```text
$ vcs config get merge.assist; echo "exit=$?"
error: merge.assist is not set
exit=1
$ vcs merge mover --assist ~/guide-run/fake-assistant.sh
automatic merge stopped: 1 conflict(s)
  conflict: lib.rs
  assist: lib.rs: proposal saved

1 proposal(s) saved, none applied. Review with `vcs conflict proposals`, apply with `vcs resolve <path> --proposal`.

Resolve the files, `vcs add` each one, then `vcs commit`.
Inspect with `vcs conflict list|show <path>`; give up with `vcs merge --abort`.
error: merge has conflicts
[exit 1]
$ vcs conflict proposals
proposal for lib.rs
  assistant says: kept the editor branch value for alpha
  diff from our side:
      ...
  apply with: vcs resolve lib.rs --proposal
$ cat lib.rs | head -8
fn gamma() -> i32 {
    3
}

fn alpha() -> i32 {
<<<<<<< ours (root/editor)
    111
||||||| base
$ vcs resolve lib.rs --proposal
resolved lib.rs from the assistant's proposal
0 conflict(s) left
$ cat lib.rs
fn alpha() -> i32 {
    111
}

fn beta() -> i32 {
    2
}

fn gamma() -> i32 {
    33
}
$ vcs commit -m "merge mover, assisted"
[root/editor 4ad3767bc5dc] merge mover, assisted
```

Note that the file still had conflict markers after the assistant ran. Nothing is applied until you say so.
If the answer is bad (markers in it, not JSON, too slow), it is ignored and the conflict is left as it was.

Next: [chapter 10, operating a server](10-operating-a-server.md).
