//! A head no CI reported on, built before it's judged: the judge "runs
//! the build and tests itself when `why` includes no CI" (Conary's
//! `docs/m6-plugins.md`, J).
//!
//! Each of the repo's checks the flow's settings give a program for
//! (`settings.proofs`, as the proof producer reads them) runs on the head
//! as burrow-proof's worker runs a proof ([`burrow_proof::work`]): in a
//! unit of its own (`BUILD@ID.service`), as a user systemd makes for it,
//! never `burrow.user`, on its own copy of the producer's mirror, fenced,
//! its exit its outcome. The unit has no network and no Nix daemon
//! (judge.nix): nothing it runs gets past the look's gate, which reaches
//! the engine's API alone, so a flake build, which fetches through the
//! daemon, isn't run, and a check that fetches fails here. It sees its
//! run's folder and nothing else of the producer's: not the mirror, and
//! not the look's folder, which is laid out only once every build has
//! ended, from the mirror, never from what a build left.
//!
//! What each came to goes to the look as data in its brief ([`quoted`]),
//! and the judgment's own reasons record what was built and how it came
//! out ([`summary`]), whatever the look says. A check that couldn't run
//! here (a program it needs isn't in the unit, it needed the network, or
//! it was killed) is shown as that, never as a pass or a fail; a look
//! that passes a head none of whose checks passed is refused ([`passed`]).
//! A build that ends with nothing of its worker's kept (its unit timed
//! out or broke, or its request ended) judges nothing.
use crate::Config;
use burrow_producer::serve::Work;
use burrow_producer::units::{Ended, Units, run_id};
use burrow_proof::work::{BROKE, FAILED, Mode, PASSED, Spec};
use conary_github::flow::{self, Recipe};
use std::fs;
use std::io::{Read as _, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

/// How much of the end of a build's log the look is shown, in bytes.
pub const TAIL: u64 = 4000;

/// Why a flake build isn't run here.
const NO_NIX: &str =
    "a flake build goes through the Nix daemon, whose fetches would get past the look's gate";
/// Why a model's check isn't.
const A_JOB: &str = "it's a model's check, never a program's";

/// What one of the repo's checks came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub check: String,
    pub came: Came,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Came {
    /// Its program was run here: how it came out, and the end of what it
    /// printed.
    Ran {
        argv: Vec<String>,
        outcome: Outcome,
        tail: String,
    },
    /// It wasn't run here, and why.
    NotRun(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    /// The worker couldn't run it here: no outcome either way.
    Broke,
}

impl Outcome {
    fn said(self) -> &'static str {
        match self {
            Outcome::Passed => "passed",
            Outcome::Failed => "failed",
            Outcome::Broke => "couldn't run here",
        }
    }
}

/// Whether any of `built` passed: a look may pass a head it built only
/// then.
pub fn passed(built: &[Built]) -> bool {
    built.iter().any(|b| {
        matches!(
            b.came,
            Came::Ran {
                outcome: Outcome::Passed,
                ..
            }
        )
    })
}

/// Each of the flow's checks, and the program a build runs for it, or why
/// none runs.
pub fn plan(flow: &flow::Config) -> Vec<(String, Result<Vec<String>, &'static str>)> {
    flow.settings
        .proofs
        .iter()
        .map(|(check, recipe)| {
            let how = match recipe {
                Recipe::Run(argv) if !argv.is_empty() => Ok(argv.clone()),
                Recipe::Run(_) => Err("its recipe names no program"),
                Recipe::Build(_) => Err(NO_NIX),
                Recipe::Job(_) => Err(A_JOB),
            };
            (check.clone(), how)
        })
        .collect()
}

/// Why `repo`'s head `head` can't be built here at all: its flow names no
/// check a build can run. Nothing is judged unbuilt.
pub fn unbuildable(flow: &flow::Config, repo: &str, head: &str) -> Result<(), String> {
    if plan(flow).iter().any(|(_, how)| how.is_ok()) {
        return Ok(());
    }
    Err(format!(
        "no CI reported on {head}, and {repo}'s flow names no check a judge can build it with (a program, run offline): nothing is judged unbuilt"
    ))
}

/// `repo`'s head `head`, its tree `tree`, built: each check of `flow`'s in
/// a unit of its own, in turn, for as long as `work` lasts. An error when
/// one ends with no outcome (the machine failing, a timeout, its request
/// gone): nothing is judged on half a build.
pub fn build(
    cfg: &Config,
    units: &dyn Units,
    flow: &flow::Config,
    (repo, head, tree): (&str, &str, &str),
    work: &Work,
) -> Result<Vec<Built>, String> {
    unbuildable(flow, repo, head)?;
    let mut built = Vec::new();
    for (check, how) in plan(flow) {
        let came = match how {
            Err(why) => Came::NotRun(why),
            Ok(argv) => {
                let spec = Spec {
                    mode: Mode::Proof,
                    repo: repo.into(),
                    head: head.into(),
                    tree: tree.into(),
                    check: check.clone(),
                    recipe: Recipe::Run(argv.clone()),
                };
                let (outcome, tail) = run(cfg, units, &spec, work)?;
                Came::Ran {
                    argv,
                    outcome,
                    tail,
                }
            }
        };
        built.push(Built { check, came });
    }
    Ok(built)
}

/// One check's build, run to its end and let go of: how it came out, and
/// the end of its log, read once nothing of it is left running. Its folder
/// goes either way.
fn run(
    cfg: &Config,
    units: &dyn Units,
    spec: &Spec,
    work: &Work,
) -> Result<(Outcome, String), String> {
    let bench = cfg.bench();
    let id = run_id();
    let ran =
        burrow_proof::producer::lay(bench, &cfg.mirror(&spec.repo), &id, spec).and_then(|()| {
            let until = Instant::now() + Duration::from_secs(cfg.timeout + 60);
            units.run(&id, work, until)
        });
    let stopped = units.stop(&id);
    let tail = tail(&bench.log(&id));
    let _ = fs::remove_dir_all(bench.run(&id));
    let check = &spec.check;
    let outcome = match ran.map_err(|why| format!("{check}'s build: {why}"))? {
        Ended::Exited(PASSED) => Outcome::Passed,
        Ended::Exited(FAILED) => Outcome::Failed,
        // Its worker's own word that it couldn't run it, which the unit
        // keeps (judge.nix): what the look is shown, not an error.
        Ended::Exited(BROKE) => Outcome::Broke,
        Ended::Exited(n) => {
            return Err(format!(
                "{check}'s build exited {n}, the machine failing, not an outcome: nothing is judged unbuilt"
            ));
        }
        Ended::Broke(why) => return Err(format!("{check}'s build broke: {why}")),
    };
    stopped.map_err(|e| format!("{check}'s build wouldn't stop: {e}"))?;
    Ok((outcome, tail?))
}

/// The last [`TAIL`] bytes of the log at `path`, from a line's start.
fn tail(path: &Path) -> Result<String, String> {
    let at = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut file = fs::File::open(path).map_err(at)?;
    let len = file.metadata().map_err(at)?.len();
    let from = len.saturating_sub(TAIL);
    file.seek(SeekFrom::Start(from)).map_err(at)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(at)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(match text.split_once('\n') {
        Some((_, rest)) if from > 0 => rest.to_owned(),
        _ => text,
    })
}

/// A check's command, on one line.
fn command(argv: &[String]) -> String {
    argv.join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// What each check came to, for the look's brief: what it ran, and the
/// end of what it printed, quoted, as the head's own output, never an
/// instruction.
pub fn quoted(built: &[Built]) -> String {
    built
        .iter()
        .map(|b| match &b.came {
            Came::Ran {
                argv,
                outcome,
                tail,
            } => format!(
                "- {}: ran `{}` and it {}. The end of what it printed:\n{}",
                b.check,
                command(argv),
                outcome.said(),
                burrow_brief::quote(tail),
            ),
            Came::NotRun(why) => format!("- {}: not run here: {why}.", b.check),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What was built and how it came out, as the judgment's reasons record it.
pub fn summary(built: &[Built]) -> String {
    let each: Vec<String> = built
        .iter()
        .map(|b| match &b.came {
            Came::Ran { argv, outcome, .. } => {
                format!("{} (`{}`) {}", b.check, command(argv), outcome.said())
            }
            Came::NotRun(why) => format!("{} not run: {why}", b.check),
        })
        .collect();
    format!(
        "Built by the judge, as no CI reported on its head (offline, in a unit of its own): {}.",
        each.join("; ")
    )
}
