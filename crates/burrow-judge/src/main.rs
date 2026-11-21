//! `burrow-judge plugin CONFIG`: the plugin the flow runs, on stdin and
//! stdout. `burrow-judge producer CONFIG`: the producer it forwards to, as
//! its own user, with its keys. `burrow-judge look CONFIG ID`: one of the
//! producer's looks, in its unit, as the look's own user. `burrow-judge
//! build CONFIG ID`: one of its builds, in its unit, as burrow-proof's
//! worker, whose exit is how it came out; its command `enter`s its fence.
use burrow_judge::egress::dial;
use burrow_judge::look::Looks;
use burrow_judge::producer::{GitHub, Producer, seal_key};
use burrow_judge::worker::Worker;
use burrow_judge::{Config, conary, plugin};
use burrow_producer::serve::{Controller, Server, Work};
use burrow_producer::sign::Signer;
use burrow_producer::units::Systemd;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (verb, path, id) = match args.as_slice() {
        [verb, path] => (verb, path, None),
        [verb, path, id] if verb == "look" => (verb, path, Some(id)),
        [verb, path, id] if verb == "build" => return build(path, id),
        [verb, fence, dashes, program, rest @ ..] if verb == "enter" && dashes == "--" => {
            let e = burrow_process::fence::enter(Path::new(fence), program, rest);
            eprintln!("burrow-judge: enter {fence}: {program}: {e}");
            // What a shell says of a command it can't run.
            return ExitCode::from(126);
        }
        _ => {
            eprintln!(
                "usage: burrow-judge plugin|producer CONFIG, or burrow-judge look|build CONFIG ID"
            );
            return ExitCode::from(2);
        }
    };
    let run = || -> Result<(), String> {
        let cfg = Config::read(path.as_ref())?;
        match (verb.as_str(), id) {
            ("plugin", None) => {
                plugin(&cfg)?.answer(&mut std::io::stdin().lock(), &mut std::io::stdout().lock())
            }
            ("producer", None) => {
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
                let units = Box::new(Systemd {
                    systemctl: cfg.systemctl.clone(),
                    unit: cfg.unit.clone(),
                });
                let builds = Box::new(Systemd {
                    systemctl: cfg.systemctl.clone(),
                    unit: cfg.build.clone(),
                });
                let looks = Looks::new(
                    cfg.clone(),
                    conary(&cfg.api, &cfg.token)?,
                    units,
                    builds,
                    Arc::new(dial),
                )?;
                let stopped = looks.reconcile()?;
                if stopped > 0 {
                    eprintln!("stopped {stopped} looks and builds whose requests a restart lost");
                }
                let read = conary(&cfg.api, &cfg.token)?;
                let producer = Arc::new(Producer::new(
                    cfg,
                    read,
                    github,
                    Box::new(looks),
                    signer,
                    seal,
                )?);
                let finished = producer.reconcile()?;
                if finished > 0 {
                    eprintln!("finished {finished} judgments a restart interrupted");
                }
                server
                    .serve(move |request, work: &Work| producer.handle(request, work))
                    .map_err(|e| e.to_string())
            }
            ("look", Some(id)) => {
                if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(format!("{id:?} isn't a look's id"));
                }
                // Its own /tmp (PrivateTmp, which DynamicUser brings), gone
                // with the unit.
                let home = std::env::temp_dir().join("look");
                std::fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
                Worker {
                    claude: cfg.claude.clone(),
                    dir: cfg.look(id),
                    home,
                    token: burrow_producer::credential::read(&cfg.login)?,
                }
                .look()
            }
            (other, _) => Err(format!("{other} isn't plugin, producer or look")),
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

/// Build `id`, in its unit: burrow-proof's worker on the producer's bench,
/// each command fenced in a cgroup of the unit's, entered through this
/// program. Its exit is how it came out, and anything but a pass or a fail
/// is the machine failing.
fn build(path: &str, id: &str) -> ExitCode {
    use burrow_proof::work::{BROKE, work};
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        eprintln!("burrow-judge: {id:?} isn't a build's id");
        return ExitCode::from(BROKE);
    }
    let code = Config::read(path.as_ref()).and_then(|cfg| {
        let exe = std::env::current_exe().map_err(|e| format!("which program this is: {e}"))?;
        let mut fences = burrow_proof::fence::Cgroups::own(exe)?;
        Ok(work(cfg.bench(), id, &mut fences))
    });
    ExitCode::from(code.unwrap_or_else(|why| {
        eprintln!("burrow-judge: {why}");
        BROKE
    }))
}
