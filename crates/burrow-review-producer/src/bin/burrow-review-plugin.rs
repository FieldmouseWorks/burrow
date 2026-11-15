//! `burrow-review-plugin plugin CONFIG`: Conary's review plugin, on stdin
//! and stdout. `burrow-review-plugin producer CONFIG`: the producer it
//! forwards to, which reads GitHub as the App its gh is and has the
//! reviewers' service run its looks. `burrow-review-plugin sign CONFIG`:
//! the sign pass, which holds the key (its systemd credential) and signs
//! each look's publication.
use burrow_producer::serve::{Controller, Server, Work};
use burrow_producer::sign::Signer;
use burrow_review_producer::look::Queue;
use burrow_review_producer::producer::{GitHub, Producer, Upkeep};
use burrow_review_producer::{Config, plugin, sign};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [verb, path] = args.as_slice() else {
        eprintln!("usage: burrow-review-plugin plugin|producer|sign CONFIG");
        return ExitCode::from(2);
    };
    let run = || -> Result<(), String> {
        let cfg = Config::read(path.as_ref())?;
        let gh = cfg.gh.clone();
        let github: GitHub = Box::new(move || {
            Box::new(burrow_github::Gh {
                program: Some(gh.clone().into_os_string()),
                ..Default::default()
            })
        });
        match verb.as_str() {
            "plugin" => {
                plugin(&cfg)?.answer(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
            }
            "producer" => {
                let controller = Controller::named(&cfg.controller)?;
                let server = Server::bind(&cfg.socket, controller)
                    .map_err(|e| format!("{}: {e}", cfg.socket.display()))?;
                let snapshots = Upkeep::new(&cfg.upkeep, &cfg.state, &cfg.fresh)?;
                let owner = snapshots.cfg.owner.clone();
                let looker = Queue {
                    queue: cfg.queue.clone(),
                    done: cfg.done.clone(),
                    every: Duration::from_secs(1),
                };
                let producer = Arc::new(Producer::new(
                    cfg,
                    github,
                    Box::new(snapshots),
                    Box::new(looker),
                    owner,
                )?);
                let gone = producer.reconcile()?;
                if gone > 0 {
                    eprintln!("let go of {gone} looks a restart left");
                }
                server
                    .serve(move |request, work: &Work| producer.handle(request, work))
                    .map_err(|e| e.to_string())
            }
            "sign" => {
                let signer = Signer::load(&cfg.producer, &cfg.credential)?;
                let passed = sign::pass(&cfg, &mut *github(), &signer)?;
                if passed.finished > 0 {
                    eprintln!("finished {} signatures a restart left", passed.finished);
                }
                for id in &passed.signed {
                    eprintln!("signed look {id}'s publication");
                }
                if !passed.failed.is_empty() {
                    eprintln!(
                        "{} things failed; the next pass is booked",
                        passed.failed.len()
                    );
                }
                if !passed.waiting.is_empty() {
                    eprintln!("{} looks wait for their publication", passed.waiting.len());
                }
                Ok(())
            }
            other => Err(format!("{other} isn't plugin, producer or sign")),
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-review-plugin: {e}");
            ExitCode::FAILURE
        }
    }
}
