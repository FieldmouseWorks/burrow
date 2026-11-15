//! `burrow-fix plugin CONFIG`: the plugin the flow runs, on stdin and
//! stdout. `burrow-fix broker CONFIG`: the broker it forwards to.
use burrow_fix::broker::Broker;
use burrow_fix::jobs::Proj;
use burrow_fix::{Config, plugin};
use burrow_producer::serve::{Controller, Server};
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [verb, path] = args.as_slice() else {
        eprintln!("usage: burrow-fix plugin|broker CONFIG");
        return ExitCode::from(2);
    };
    let run = || -> Result<(), String> {
        let cfg = Config::read(path.as_ref())?;
        match verb.as_str() {
            "plugin" => {
                plugin(&cfg)?.answer(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
            }
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
                let broker = Arc::new(
                    Broker::new(cfg.clone(), Box::new(Proj(cfg)), github)
                        .map_err(|e| e.to_string())?,
                );
                let stopped = broker.reconcile()?;
                if stopped > 0 {
                    eprintln!("stopped {stopped} fix jobs whose requests a restart lost");
                }
                server
                    .serve(move |request, work: &burrow_producer::serve::Work| {
                        broker.handle(request, work)
                    })
                    .map_err(|e| e.to_string())
            }
            other => Err(format!("{other} isn't plugin or broker")),
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
