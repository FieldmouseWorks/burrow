//! flaky: the GitHub side of quarantining flaky tests (burrow's
//! docs/flaky-tests.md). It talks to GitHub through gh, with GH_TOKEN or
//! gh's own login.

use std::process::ExitCode;

use flaky::github::Gh;
use flaky::quarantine::{self, default_cache};
use flaky::{actions, filing, report, run};

const USAGE: &str = "usage:
  flaky quarantine OWNER/REPO [--json]
      the tests OWNER/REPO quarantines, one a line; with --json, one
      {\"test\", \"issue\", \"author\"} a line, every open issue
  flaky file OWNER/REPO REPORT [--where MARKDOWN]
      a gate's report on its tests' issues
  flaky run OWNER/REPO [--where MARKDOWN] -- COMMAND [ARG...]
      COMMAND (a gate) with FLAKY_QUARANTINE and FLAKY_REPORT set, then
      its report filed
  flaky action start|file
      the same around a gate, as a step of a GitHub Actions job";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match go(&args) {
        Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
        Err(Usage(why)) => {
            if !why.is_empty() {
                eprintln!("flaky: {why}");
            }
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

struct Usage(String);

fn go(args: &[String]) -> Result<i32, Usage> {
    let cache = default_cache();
    let cache = cache.as_deref();
    let mut gh = Gh;
    let fail = |e: String| {
        eprintln!("flaky: {e}");
        1
    };
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["quarantine", repo, rest @ ..] => {
            let json = match rest {
                [] => false,
                ["--json"] => true,
                _ => return Err(Usage(format!("unexpected {rest:?}"))),
            };
            let repo = repo_arg(repo)?;
            Ok(match quarantine::list(&mut gh, repo, cache) {
                Ok(q) if json => {
                    for x in q {
                        println!(
                            "{}",
                            serde_json::json!({"test": x.test, "issue": x.issue, "author": x.author})
                        );
                    }
                    0
                }
                Ok(q) => {
                    print!("{}", run::lines(&q));
                    0
                }
                Err(e) => fail(e.to_string()),
            })
        }
        ["file", repo, path, rest @ ..] => {
            let repo = repo_arg(repo)?;
            let place = match rest {
                [] => format!("a run whose report is `{path}`"),
                ["--where", w] => w.to_string(),
                _ => return Err(Usage(format!("unexpected {rest:?}"))),
            };
            let entries = match std::fs::read_to_string(path)
                .map_err(|e| e.to_string())
                .and_then(|t| report::parse(&t))
            {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("flaky: {path}: {e}");
                    return Ok(2);
                }
            };
            if entries.is_empty() {
                return Ok(0);
            }
            Ok(
                match quarantine::list(&mut gh, repo, cache)
                    .and_then(|open| filing::file(&mut gh, repo, &entries, &open, &place))
                {
                    Ok(filed) => {
                        for f in filed {
                            println!(
                                "{} {}: {repo}#{}",
                                f.test,
                                if f.new { "quarantined by" } else { "noted on" },
                                f.issue
                            );
                        }
                        0
                    }
                    Err(e) => fail(e.to_string()),
                },
            )
        }
        ["run", repo, rest @ ..] => {
            let repo = repo_arg(repo)?;
            let (place, cmd) = match rest {
                ["--where", w, "--", cmd @ ..] => (Some(*w), cmd),
                ["--", cmd @ ..] => (None, cmd),
                _ => return Err(Usage("run takes -- and the command".into())),
            };
            if cmd.is_empty() {
                return Err(Usage("no command to run".into()));
            }
            let cmd: Vec<String> = cmd.iter().map(|s| s.to_string()).collect();
            Ok(run::run(&mut gh, repo, place, &cmd, cache))
        }
        ["action", step] => {
            let env = |k: &str| std::env::var(k).ok();
            let done = match *step {
                "start" => actions::start(&mut gh, &env, cache),
                "file" => actions::file(&mut gh, &env, cache),
                _ => return Err(Usage(format!("no action step {step:?}: start or file"))),
            };
            Ok(match done {
                Ok(()) => 0,
                Err(e) => {
                    println!("::error title=Flaky tests::{}", actions::data(&e.0));
                    1
                }
            })
        }
        [] | ["-h" | "--help" | "help", ..] => Err(Usage(String::new())),
        _ => Err(Usage(format!("unknown command {:?}", args[0]))),
    }
}

/// OWNER/REPO, as GitHub allows them: it goes into paths.
fn repo_arg(r: &str) -> Result<&str, Usage> {
    let ok = |s: &str, extra: &str| {
        !s.is_empty()
            && !s.starts_with(['.', '-'])
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-".contains(c) || extra.contains(c))
    };
    match r.split_once('/') {
        Some((o, n)) if ok(o, "") && ok(n, "_.") => Ok(r),
        _ => Err(Usage(format!("{r:?} isn't OWNER/REPO"))),
    }
}
