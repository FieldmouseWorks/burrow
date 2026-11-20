//! `burrow-proof-plugin plugin CONFIG`: Conary's proof plugin, on stdin and
//! stdout. `burrow-proof-plugin producer CONFIG`: the producer it forwards
//! to, which holds the key (its systemd credential) and acts on GitHub as
//! the App its gh is. `burrow-proof-plugin choice`: what the flow's
//! configuration asks it to run with (its `execution`), as JSON: its engine,
//! a program's, with no model or effort.
use burrow_producer::serve::{Controller, Server, Work};
use burrow_producer::sign::Signer;
use burrow_producer::units::Systemd;
use burrow_proof::plugin::{Config, plugin};
use burrow_proof::producer::{GitHub, Producer};
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args == ["choice"] {
        println!("{}", burrow_proof::plugin::choice());
        return ExitCode::SUCCESS;
    }
    let [verb, path] = args.as_slice() else {
        eprintln!("usage: burrow-proof-plugin plugin|producer CONFIG | burrow-proof-plugin choice");
        return ExitCode::from(2);
    };
    let run = || -> Result<(), String> {
        let cfg = Config::read(path.as_ref())?;
        match verb.as_str() {
            "plugin" => {
                plugin(&cfg).answer(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
            }
            "producer" => {
                let signer = Signer::load(&cfg.producer, &cfg.credential)?;
                let controller = Controller::named(&cfg.controller)?;
                let server = Server::bind(&cfg.socket, controller)
                    .map_err(|e| format!("{}: {e}", cfg.socket.display()))?;
                let gh = cfg.gh.clone();
                let github: GitHub = Box::new(move || {
                    Box::new(burrow_github::Gh {
                        program: Some(gh.clone().into_os_string()),
                        ..Default::default()
                    })
                });
                let units = Box::new(Systemd {
                    systemctl: cfg.systemctl.clone(),
                    unit: cfg.unit.clone(),
                });
                let producer = Arc::new(Producer::new(cfg, github, units, signer)?);
                let finished = producer.reconcile()?;
                if finished > 0 {
                    eprintln!("finished {finished} proofs a restart left");
                }
                server
                    .serve(move |request, work: &Work| producer.handle(request, work))
                    .map_err(|e| e.to_string())
            }
            other => Err(format!("{other} isn't plugin or producer")),
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-proof-plugin: {e}");
            ExitCode::FAILURE
        }
    }
}
