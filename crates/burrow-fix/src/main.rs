//! `burrow-fix plugin CONFIG`: the plugin the flow runs for fixes, on stdin
//! and stdout. `burrow-fix job CONFIG`: the one it runs for the stuck
//! role's `job`. `burrow-fix broker CONFIG`: the broker both forward to.
use burrow_fix::broker::Broker;
use burrow_fix::{Config, job_plugin, plugin, proj, stuck_proj};
use burrow_producer::serve::{Controller, Server};
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [verb, path] = args.as_slice() else {
        eprintln!("usage: burrow-fix plugin|job|broker CONFIG");
        return ExitCode::from(2);
    };
    let run = || -> Result<(), String> {
        let cfg = Config::read(path.as_ref())?;
        match verb.as_str() {
            "plugin" => {
                plugin(&cfg)?.answer(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
            }
            "job" => job_plugin(&cfg)?
                .answer(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()),
            "broker" => {
                let controller = Controller::named(&cfg.controller)?;
                let server = Server::bind(&cfg.socket, controller)
                    .map_err(|e| format!("{}: {e}", cfg.socket.display()))?;
                let gh = cfg.gh.clone();
                let github: burrow_fix::broker::GitHub = Box::new(move || {
                    Box::new(burrow_github::Gh {
                        program: Some(gh.clone().into_os_string()),
                        ..Default::default()
                    })
                });
                let mut broker = Broker::new(cfg.clone(), Box::new(proj(&cfg)), github)
                    .map_err(|e| e.to_string())?;
                broker.stuck = stuck_proj(&cfg).map(|p| Box::new(p) as _);
                let broker = Arc::new(broker);
                let held = broker.reconcile()?;
                if held > 0 {
                    eprintln!("holds {held} fix jobs from before the restart, at work or ended");
                }
                server
                    .serve(move |request, work: &burrow_producer::serve::Work| {
                        broker.handle(request, work)
                    })
                    .map_err(|e| e.to_string())
            }
            other => Err(format!("{other} isn't plugin, job or broker")),
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-fix: {e}");
            ExitCode::FAILURE
        }
    }
}
