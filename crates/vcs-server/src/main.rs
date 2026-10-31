//! `vcs-server --root <dir> --tokens <file> [--listen 127.0.0.1:8480]`
//! `vcs-server hash-token <token>` prints the hash to put in the tokens file.
//! No TLS: run behind a TLS-terminating proxy (docs/server.md).

use std::sync::Arc;
use vcs_remote::FsBackend;
use vcs_server::{auth, Config, Server, TokenTable};

fn die(m: &str) -> ! {
    eprintln!("vcs-server: {m}");
    std::process::exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("hash-token") {
        match args.get(1) {
            Some(t) => println!("{}", auth::hash_token(t)),
            None => die("usage: vcs-server hash-token <token>"),
        }
        return;
    }
    let (mut root, mut tokens, mut listen) = (None, None, "127.0.0.1:8480".to_owned());
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        let mut val = |n: &str| it.next().unwrap_or_else(|| die(&format!("{n} needs a value")));
        match a.as_str() {
            "--root" => root = Some(val("--root")),
            "--tokens" => tokens = Some(val("--tokens")),
            "--listen" => listen = val("--listen"),
            _ => die("usage: vcs-server --root <dir> --tokens <file> [--listen host:port] | hash-token <token>"),
        }
    }
    let root = root.unwrap_or_else(|| die("--root is required"));
    let tokens = tokens.unwrap_or_else(|| die("--tokens is required"));
    let text = std::fs::read_to_string(&tokens).unwrap_or_else(|e| die(&format!("cannot read {tokens}: {e}")));
    let table = TokenTable::parse(&text).unwrap_or_else(|e| die(&e));
    if table.is_empty() {
        die("tokens file has no entries");
    }
    let backend = FsBackend::open(&root).unwrap_or_else(|e| die(&format!("cannot open {root}: {e}")));
    let srv =
        Server::start(Arc::new(backend), Config::new(table), &listen).unwrap_or_else(|e| die(&format!("cannot listen on {listen}: {e}")));
    eprintln!(
        "vcs-server listening on http://{} (no TLS: use a TLS proxy beyond loopback)",
        srv.local_addr()
    );
    srv.join();
}
