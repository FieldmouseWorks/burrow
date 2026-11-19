//! `burrow-landing CONFIG`: one run (lib.rs says what it does), started by
//! modules/landing.nix whenever the settings or the steward's release
//! marker change, at boot and at each deploy.
use std::process::ExitCode;
use std::time::Duration;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [config] = args.as_slice() else {
        eprintln!("usage: burrow-landing CONFIG");
        return ExitCode::from(2);
    };
    let result = std::fs::read(config)
        .map_err(|e| format!("{config}: {e}"))
        .and_then(|b| {
            serde_json::from_slice::<burrow_landing::Config>(&b)
                .map_err(|e| format!("{config}: {e}"))
        })
        .and_then(|cfg| {
            burrow_landing::run(
                &cfg,
                &mut burrow_landing::Kick(&cfg.steward_kick),
                // A pass is minutes at most; a worker is waited for until
                // it exits, under the unit's own timeout.
                &burrow_landing::Patience {
                    lock: Duration::from_secs(600),
                },
            )
        });
    match result {
        Ok(moved) => {
            for (repo, to) in &moved.started {
                println!("{repo}: handing its landing over to {}", to.name());
            }
            for repo in &moved.undone {
                println!("{repo}: set back before its handover was done; its owner carries on");
            }
            for (repo, to) in &moved.moved {
                println!("{repo}: landed by {} from now", to.name());
            }
            for repo in &moved.waiting {
                println!("{repo}: waiting for its owner to let go of its work there");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("burrow-landing: {e}");
            ExitCode::FAILURE
        }
    }
}
