use std::io::{Read, Write};
use std::process::ExitCode;
use vcs_assist_claude::{eval, parse_request, resolve, response_json, Outcome};
use vcs_assist_openai::api::{ApiConfig, Client};

const HELP: &str = "vcs-assist-openai: assistant for `vcs merge --assist` using an OpenAI-compatible API.\n\n\
usage:\n  vcs-assist-openai                         read a request on stdin, write a response on stdout\n  \
vcs-assist-openai eval <dir> [--live] [--json]   score the assistant on fixtures\n\n\
File contents are sent to the API. Needs OPENAI_API_KEY; model from VCS_ASSIST_MODEL (default gpt-4.1-mini);\n\
endpoint from VCS_ASSIST_API_URL; hard budget from VCS_ASSIST_MAX_REQUESTS / VCS_ASSIST_MAX_TOTAL_TOKENS.\nSee docs/assist.md.\n";

fn env(k: &str) -> Option<String> {
    std::env::var(k).ok()
}

fn fail(msg: &str) -> ExitCode {
    let key = env("OPENAI_API_KEY").unwrap_or_default();
    let m = if key.is_empty() {
        msg.to_owned()
    } else {
        msg.replace(&key, "[redacted]")
    };
    eprintln!("vcs-assist-openai: {m}");
    ExitCode::from(1)
}

fn usage_line(c: &Client) -> String {
    let u = c.usage();
    format!(
        "usage: requests={} prompt_tokens={} completion_tokens={} model={}",
        u.requests, u.prompt_tokens, u.completion_tokens, c.cfg.model
    )
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
            eprintln!("vcs-assist-openai: unknown argument {other:?}\n{HELP}");
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
    let cfg = match ApiConfig::from_env(&env) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let client = Client::new(cfg);
    let out = resolve(&req, &|s, u, m| client.complete(s, u, m));
    eprintln!("vcs-assist-openai: {}", usage_line(&client));
    match out {
        Outcome::Proposal {
            resolution, explanation, ..
        } => {
            let key = &client.cfg.key;
            if resolution.contains(key.as_str()) {
                return fail("proposal rejected: it contains the API key");
            }
            let explanation = explanation.replace(key.as_str(), "[redacted]");
            let _ = writeln!(std::io::stdout(), "{}", response_json(&resolution, &explanation));
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
                eprintln!("vcs-assist-openai eval: unexpected argument {s:?}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: vcs-assist-openai eval <dir> [--live] [--json]");
        return ExitCode::from(2);
    };
    let (rep, client) = if live {
        let cfg = match ApiConfig::from_env(&env) {
            Ok(c) => c,
            Err(e) => return fail(&e),
        };
        let client = Client::new(cfg);
        let r = eval::run(dir.as_ref(), Some(&|s: &str, u: &str, m: u32| client.complete(s, u, m)));
        (r, Some(client))
    } else {
        (eval::run(dir.as_ref(), None), None)
    };
    if let Some(c) = &client {
        eprintln!("vcs-assist-openai: {}", usage_line(c));
    }
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
