//! `burrow-ledger-door CONFIG`: a pass over the repo's undo issues, which
//! burrow-events runs on the repo's issue events (modules/ledger.nix).

use std::fs;
use std::process::{Command, ExitCode};

use burrow_github::deferred;
use burrow_ledger_door::Config;
use burrow_ledger_door::pass::{Cn, Ran, pass};

/// `cn` as the config names it.
struct Run<'a>(&'a Config);

impl Cn for Run<'_> {
    fn run(&self, args: &[String]) -> Result<Ran, String> {
        let out = Command::new(&self.0.cn)
            .args(args)
            .output()
            .map_err(|e| format!("{}: {e}", self.0.cn.display()))?;
        Ok(Ran {
            code: out.status.code(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [config] = args.as_slice() else {
        eprintln!("usage: burrow-ledger-door CONFIG");
        return ExitCode::from(2);
    };
    let cfg: Config = match fs::read_to_string(config)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("burrow-ledger-door: {config}: {e}");
            return ExitCode::from(2);
        }
    };
    match deferred::configured(|api| pass(api, &cfg, &Run(&cfg))) {
        Ok(Some(done)) => {
            for n in &done.answered {
                eprintln!("undid {}#{n}", cfg.repo);
            }
            for n in &done.refused {
                eprintln!("turned away {}#{n}", cfg.repo);
            }
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-ledger-door: {e}");
            ExitCode::FAILURE
        }
    }
}
