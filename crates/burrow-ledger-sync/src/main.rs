//! `burrow-ledger-sync CONFIG [OWNER/REPO]`: the ledger's pass, which
//! burrow-events runs on a repo's PR events (modules/ledger.nix).

use std::fs;
use std::process::{Command, ExitCode};

use burrow_ledger_sync::{Cn, Config, pass};

/// `cn` as the config names it.
struct Run<'a>(&'a Config);

impl Cn for Run<'_> {
    fn run(&self, args: &[&str]) -> Result<Option<i32>, String> {
        Command::new(&self.0.cn)
            .args(args)
            .status()
            .map(|s| s.code())
            .map_err(|e| format!("{}: {e}", self.0.cn.display()))
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (config, only) = match args.as_slice() {
        [c] => (c, None),
        [c, r] => (c, Some(r.as_str())),
        _ => {
            eprintln!("usage: burrow-ledger-sync CONFIG [OWNER/REPO]");
            return ExitCode::from(2);
        }
    };
    let cfg: Config = match fs::read_to_string(config)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("burrow-ledger-sync: {config}: {e}");
            return ExitCode::from(2);
        }
    };
    match pass(
        &cfg,
        &mut burrow_github::Gh::default(),
        &Run(&cfg),
        only,
        &now,
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-ledger-sync: {e}");
            ExitCode::FAILURE
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
