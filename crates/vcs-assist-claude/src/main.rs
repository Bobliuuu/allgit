use std::io::{Read, Write};
use std::process::ExitCode;
use vcs_assist_claude::{api, eval, parse_request, resolve, response_json, Outcome};

const HELP: &str = "vcs-assist-claude: assistant for `vcs merge --assist`.\n\n\
usage:\n  vcs-assist-claude                         read a request on stdin, write a response on stdout\n  \
vcs-assist-claude eval <dir> [--live] [--json]   score the assistant on recorded fixtures\n\n\
File contents are sent to the Anthropic API. Needs ANTHROPIC_API_KEY; model from VCS_ASSIST_MODEL.\nSee docs/assist.md.\n";

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

/// Print to stderr with the key scrubbed (defence in depth: nothing we format contains it).
fn fail(msg: &str) -> ExitCode {
    let key = env("ANTHROPIC_API_KEY").unwrap_or_default();
    let m = if key.is_empty() {
        msg.to_owned()
    } else {
        msg.replace(&key, "[redacted]")
    };
    eprintln!("vcs-assist-claude: {m}");
    ExitCode::from(1)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        Some("eval") => run_eval(&args[1..]),
        Some(other) => {
            eprintln!("vcs-assist-claude: unknown argument {other:?}\n{HELP}");
            ExitCode::from(2)
        }
        None => run_assist(),
    }
}

fn run_assist() -> ExitCode {
    let mut buf = Vec::new();
    if std::io::stdin().take(16 << 20).read_to_end(&mut buf).is_err() {
        return fail("cannot read stdin");
    }
    let req = match parse_request(&buf) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    let cfg = match api::ApiConfig::from_env(&env) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let out = resolve(&req, &|s, u, m| api::complete(&cfg, s, u, m));
    match out {
        Outcome::Proposal {
            resolution, explanation, ..
        } => {
            if resolution.contains(&cfg.key) {
                return fail("proposal rejected: it contains the API key");
            }
            let explanation = explanation.replace(&cfg.key, "[redacted]");
            let mut o = std::io::stdout();
            let _ = writeln!(o, "{}", response_json(&resolution, &explanation));
            ExitCode::SUCCESS
        }
        Outcome::Refused(r) => fail(&format!("refused: {r}")),
        Outcome::Failed(r) => fail(&r),
    }
}

fn run_eval(args: &[String]) -> ExitCode {
    let (mut dir, mut live, mut json) = (None, false, false);
    for a in args {
        match a.as_str() {
            "--live" => live = true,
            "--json" => json = true,
            s if !s.starts_with('-') && dir.is_none() => dir = Some(s.to_owned()),
            s => {
                eprintln!("vcs-assist-claude eval: unexpected argument {s:?}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: vcs-assist-claude eval <dir> [--live] [--json]");
        return ExitCode::from(2);
    };
    let rep = if live {
        let cfg = match api::ApiConfig::from_env(&env) {
            Ok(c) => c,
            Err(e) => return fail(&e),
        };
        eval::run(dir.as_ref(), Some(&|s: &str, u: &str, m: u32| api::complete(&cfg, s, u, m)))
    } else {
        eval::run(dir.as_ref(), None)
    };
    match rep {
        Ok(r) => {
            if json {
                println!("{}", r.json());
            } else {
                print!("{}", r.text());
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(&e),
    }
}
