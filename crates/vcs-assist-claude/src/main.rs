use std::io::{Read, Write};
use std::process::ExitCode;
use vcs_assist_claude::{api, baseline, cli, eval, parse_request, resolve, response_json, zones, Outcome};

const HELP: &str = "vcs-assist-claude: assistant for `vcs merge --assist` and `vcs zone suggest --assist`.\n\n\
usage:\n  vcs-assist-claude                         read a request on stdin, write a response on stdout\n  \
vcs-assist-claude eval <dir> [--live [--provider api|cli] [--record] | --mock xours|xtheirs|refuse] [--split dev|test|all] [--json]   score the assistant on recorded fixtures\n\n\
File contents are sent to the Anthropic API (or, with VCS_ASSIST_PROVIDER=cli / --provider cli, to the `claude` CLI, which sends them to Anthropic). The API provider needs ANTHROPIC_API_KEY; model from VCS_ASSIST_MODEL.\nSee docs/assist.md.\n";

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
    match zones::request_kind(&buf) {
        Ok(zones::Kind::Zone) => return run_zone_assist(&buf),
        Ok(zones::Kind::Merge) => {}
        Err(e) => return fail(&e),
    }
    let req = match parse_request(&buf) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    if env("VCS_ASSIST_PROVIDER").as_deref() == Some("cli") {
        let p = match cli::CliProvider::new(cli::CliConfig::from_env(&env)) {
            Ok(p) => p,
            Err(e) => return fail(&e),
        };
        return match resolve(&req, &|s, u, m| p.complete(s, u, m)) {
            Outcome::Proposal {
                resolution, explanation, ..
            } => {
                let _ = writeln!(std::io::stdout(), "{}", response_json(&resolution, &explanation));
                ExitCode::SUCCESS
            }
            Outcome::Refused(r) => fail(&format!("refused: {r}")),
            Outcome::Failed(r) => fail(&r),
        };
    }
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

/// `"kind":"zone-suggest"`: only directory paths and metadata are in the request; same key and size rules.
fn run_zone_assist(buf: &[u8]) -> ExitCode {
    let req = match zones::parse_zone_request(buf) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    let out = if env("VCS_ASSIST_PROVIDER").as_deref() == Some("cli") {
        let p = match cli::CliProvider::new(cli::CliConfig::from_env(&env)) {
            Ok(p) => p,
            Err(e) => return fail(&e),
        };
        zones::resolve_zones(&req, &|s, u, m| p.complete(s, u, m))
    } else {
        let cfg = match api::ApiConfig::from_env(&env) {
            Ok(c) => c,
            Err(e) => return fail(&e),
        };
        let out = zones::resolve_zones(&req, &|s, u, m| api::complete(&cfg, s, u, m));
        if let zones::ZoneOutcome::Proposals { response, .. } = &out {
            if response.contains(&cfg.key) {
                return fail("answer rejected: it contains the API key");
            }
        }
        out
    };
    match out {
        zones::ZoneOutcome::Proposals { response, dropped } => {
            for d in dropped {
                eprintln!(
                    "vcs-assist-claude: dropped a proposal: {}",
                    d.replace(env("ANTHROPIC_API_KEY").unwrap_or_default().as_str(), "[redacted]")
                );
            }
            let _ = writeln!(std::io::stdout(), "{response}");
            ExitCode::SUCCESS
        }
        zones::ZoneOutcome::Refused(r) => fail(&format!("refused: {r}")),
        zones::ZoneOutcome::Failed(r) => fail(&r),
    }
}

fn run_eval(args: &[String]) -> ExitCode {
    let (mut dir, mut live, mut json, mut record, mut mock) = (None, false, false, false, None);
    let mut provider = env("VCS_ASSIST_PROVIDER").unwrap_or_else(|| "api".into());
    let mut split = eval::Split::All;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--live" => live = true,
            "--record" => record = true,
            "--mock" => match it.next().and_then(|m| baseline::Baseline::parse(m)) {
                Some(b) => mock = Some(b),
                None => {
                    eprintln!("vcs-assist-claude eval: --mock needs xours, xtheirs or refuse");
                    return ExitCode::from(2);
                }
            },
            "--json" => json = true,
            "--provider" => match it.next() {
                Some(p) if p == "api" || p == "cli" => provider = p.clone(),
                _ => {
                    eprintln!("vcs-assist-claude eval: --provider needs api or cli");
                    return ExitCode::from(2);
                }
            },
            "--split" => {
                split = match it.next().map(String::as_str) {
                    Some("dev") => eval::Split::Dev,
                    Some("test") => eval::Split::Test,
                    Some("all") => eval::Split::All,
                    _ => {
                        eprintln!("vcs-assist-claude eval: --split needs dev, test or all");
                        return ExitCode::from(2);
                    }
                }
            }
            s if !s.starts_with('-') && dir.is_none() => dir = Some(s.to_owned()),
            s => {
                eprintln!("vcs-assist-claude eval: unexpected argument {s:?}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(dir) = dir else {
        eprintln!("usage: vcs-assist-claude eval <dir> [--live [--record] | --mock xours|xtheirs|refuse] [--json]");
        return ExitCode::from(2);
    };
    if (record && !live) || (live && mock.is_some()) {
        eprintln!("vcs-assist-claude eval: --record needs --live; --mock excludes --live");
        return ExitCode::from(2);
    }
    let rep = if let Some(b) = mock {
        eval::run_split(dir.as_ref(), eval::Source::Mock(b), split)
    } else if live && provider == "cli" {
        let p = match cli::CliProvider::new(cli::CliConfig::from_env(&env)) {
            Ok(p) => p,
            Err(e) => return fail(&e),
        };
        let f = |s: &str, u: &str, m: u32| p.complete(s, u, m);
        let tm = || p.take_metrics();
        eval::run_split(
            dir.as_ref(),
            eval::Source::Live {
                complete: &f,
                record,
                metrics: Some(&tm),
            },
            split,
        )
    } else if live {
        let cfg = match api::ApiConfig::from_env(&env) {
            Ok(c) => c,
            Err(e) => return fail(&e),
        };
        let f = |s: &str, u: &str, m: u32| api::complete(&cfg, s, u, m);
        eval::run_split(
            dir.as_ref(),
            eval::Source::Live {
                complete: &f,
                record,
                metrics: None,
            },
            split,
        )
    } else {
        eval::run_split(dir.as_ref(), eval::Source::Recorded, split)
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
