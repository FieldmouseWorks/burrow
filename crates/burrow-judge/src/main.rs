//! `burrow-judge plugin CONFIG`: the plugin the flow runs, on stdin and
//! stdout. `burrow-judge producer CONFIG`: the producer it forwards to, as
//! its own user, with its keys. `burrow-judge runner CONFIG`: the runner
//! the producer has judge, as burrow.user.
use burrow_judge::producer::{GitHub, Producer, Runner as AtRunner, seal_key};
use burrow_judge::runner::Runner;
use burrow_judge::{Config, conary, plugin};
use burrow_producer::serve::{Controller, Server, Work};
use burrow_producer::sign::Signer;
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [verb, path] = args.as_slice() else {
        eprintln!("usage: burrow-judge plugin|producer|runner CONFIG");
        return ExitCode::from(2);
    };
    let run = || -> Result<(), String> {
        let cfg = Config::read(path.as_ref())?;
        match verb.as_str() {
            "plugin" => {
                plugin(&cfg)?.answer(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
            }
            "producer" => {
                let signer = Signer::load(&cfg.producer, &cfg.credential)?;
                let seal = seal_key(&cfg.seal)?;
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
                let read = conary(&cfg.api, &cfg.token)?;
                let judges = Box::new(AtRunner(cfg.runner.clone()));
                let producer = Arc::new(Producer::new(cfg, read, github, judges, signer, seal)?);
                let finished = producer.reconcile()?;
                if finished > 0 {
                    eprintln!("finished {finished} judgments a restart interrupted");
                }
                server
                    .serve(move |request, work: &Work| producer.handle(request, work))
                    .map_err(|e| e.to_string())
            }
            "runner" => {
                let user = Controller::named(&cfg.user)?;
                let server = Server::bind(&cfg.runner, user)
                    .map_err(|e| format!("{}: {e}", cfg.runner.display()))?;
                let read = conary(&cfg.api, &cfg.runner_token)?;
                let runner = Arc::new(
                    Runner::new(cfg.clone(), Box::new(cfg.proj()), read)
                        .map_err(|e| e.to_string())?,
                );
                let stopped = runner.reconcile()?;
                if stopped > 0 {
                    eprintln!("stopped {stopped} judge jobs whose requests a restart lost");
                }
                server
                    .serve(move |request, work: &Work| runner.handle(request, work))
                    .map_err(|e| e.to_string())
            }
            other => Err(format!("{other} isn't plugin, producer or runner")),
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-judge: {e}");
            ExitCode::FAILURE
        }
    }
}
