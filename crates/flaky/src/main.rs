//! flaky: the GitHub side of quarantining flaky tests (burrow's
//! docs/flaky-tests.md). It talks to GitHub through gh, with GH_TOKEN or
//! gh's own login.

use std::process::ExitCode;

use burrow_github::Gh;
use flaky::quarantine::{self, default_cache};
use flaky::{actions, run};

const USAGE: &str = "usage:
  flaky quarantine OWNER/REPO [--json]
      the tests OWNER/REPO quarantines, one a line; with --json, one
      {\"test\", \"issue\", \"author\"} a line, every open issue that quarantines
  flaky run OWNER/REPO [--where MARKDOWN] -- COMMAND [ARG...]
      COMMAND (a gate) with FLAKY_EXPIRED and FLAKY_REPORT set, then
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
            Ok(
                match quarantine::list(&mut gh, repo, cache, quarantine::now()) {
                    Ok(q) if json => {
                        for x in q.held {
                            println!(
                                "{}",
                                serde_json::json!({"test": x.test, "issue": x.issue, "author": x.author})
                            );
                        }
                        0
                    }
                    Ok(q) => {
                        print!("{}", run::lines(&quarantine::tests(&q.held)));
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
                    println!("::error title=Flaky tests::{}", actions::data(&e.why));
                    1
                }
            })
        }
        [] | ["-h" | "--help" | "help", ..] => Err(Usage(String::new())),
        _ => Err(Usage(format!("unknown command {:?}", args[0]))),
    }
}

/// OWNER/REPO, as GitHub allows them (quarantine::repo_name).
fn repo_arg(r: &str) -> Result<&str, Usage> {
    if quarantine::repo_name(r) {
        Ok(r)
    } else {
        Err(Usage(format!("{r:?} isn't OWNER/REPO")))
    }
}
