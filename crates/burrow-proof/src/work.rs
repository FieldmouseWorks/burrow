//! `burrow-proof work CONFIG ID`, a run's worker: the process its unit
//! (`UNIT@ID.service`) starts, as the worker's own user, which can read
//! the producer's mirrors and the run's folder but never its key, its
//! records or its GitHub App. It does one check's work on one commit and
//! says how it came out by its exit alone, which systemd keeps for the
//! producer ([`crate::producer`]):
//!
//! - a proof: [`PASSED`] or [`FAILED`], the check's verdict; anything else
//!   is the machine failing, never a verdict;
//! - a prepare: [`PASSED`] when it's ready and proving it needs no slot
//!   (every output is built already), [`FAILED`] when it's ready and
//!   proving it builds; anything else, not ready.
//!
//! What it prints goes to the run's log, which the producer hashes into
//! the statement it signs.
//!
//! A `Build` recipe is its flake attribute's derivation at the commit, as
//! the steward works it out (no import from derivation, no flake config),
//! realised in a heavy slot, its outputs rooted in the run's folder until
//! its unit stops. A `Run` recipe is its command line (a program and its
//! arguments, as the flow's settings give it), in a fresh clone of the
//! commit, in a fence of its own.
use crate::fence::Fence;
use crate::plugin::Config;
use crate::verdict;
use conary_github::flow::Recipe;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, fchown};
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};

/// A proof that passed, or inputs ready with no slot needed.
pub const PASSED: u8 = 0;
/// A proof that failed, or inputs ready with a slot needed.
pub const FAILED: u8 = 1;
/// The machine failing: no verdict, nothing ready.
pub const BROKE: u8 = 2;

/// What a run is: the producer writes it, and the worker only reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub mode: Mode,
    pub repo: String,
    pub head: String,
    pub tree: String,
    /// The check, as the flow's settings name it.
    pub check: String,
    pub recipe: Recipe,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Proof,
    Prepare,
}

/// The spec's file in a run's folder.
pub const SPEC: &str = "spec.json";

/// Run `id`'s work, each command fenced by `fence`: its exit.
pub fn work(cfg: &Config, id: &str, fence: &mut dyn Fence) -> u8 {
    // Its log, for the producer's group to read, as its spec is: the run's
    // own user is one systemd made for it (DynamicUser), whose group is
    // nobody else's.
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o640)
        .open(cfg.log(id))
        .and_then(|log| {
            let group = fs::metadata(cfg.run(id).join(SPEC))?.gid();
            fchown(&log, None, Some(group))?;
            Ok(log)
        });
    let log = match log {
        Ok(log) => log,
        Err(e) => {
            eprintln!("burrow-proof: {}: {e}", cfg.log(id).display());
            return BROKE;
        }
    };
    let note = |text: &str| {
        let _ = writeln!(&log, "burrow-proof: {text}");
    };
    let spec: Spec = match fs::read(cfg.run(id).join(SPEC))
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
    {
        Ok(spec) => spec,
        Err(e) => {
            note(&format!("its spec doesn't read: {e}"));
            return BROKE;
        }
    };
    let mirror = cfg.mirror(&spec.repo);
    let tree = git(cfg, &mirror)
        .args(["rev-parse", "--verify", "-q"])
        .arg(format!("{}^{{tree}}", spec.head))
        .output();
    match tree {
        Ok(o) if o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == spec.tree => {}
        _ => {
            note(&format!(
                "{} isn't in the mirror with tree {}",
                spec.head, spec.tree
            ));
            return BROKE;
        }
    }
    note(&format!("{} on {} at {}", spec.check, spec.repo, spec.head));
    match (&spec.recipe, spec.mode) {
        (Recipe::Build(attr), mode) => match derivation(cfg, &spec, &mirror, attr, &log) {
            Ok(drv) if mode == Mode::Proof => realise(cfg, id, &drv, &log),
            Ok(drv) => prepare(cfg, &drv, &log),
            Err(why) => {
                note(&why);
                BROKE
            }
        },
        (Recipe::Run(_), Mode::Prepare) => FAILED,
        (Recipe::Run(argv), Mode::Proof) => run(cfg, id, &spec, argv, fence, &log),
        (Recipe::Job(_), _) => {
            note("a job is a model's work, never a proof's");
            BROKE
        }
    }
}

fn git(cfg: &Config, dir: &Path) -> Command {
    let mut c = Command::new(&cfg.git);
    c.arg("-C").arg(dir).stdin(Stdio::null());
    c
}

/// `program` in nix's programs' directory.
fn nix(cfg: &Config, program: &str) -> Command {
    let mut c = Command::new(cfg.nix.join(program));
    c.stdin(Stdio::null());
    c
}

/// What `command` printed, its errors going to `log`, or why it failed.
fn read(mut command: Command, log: &File) -> Result<String, String> {
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let out = command
        .stderr(err)
        .output()
        .map_err(|e| format!("{:?}: {e}", command.get_program()))?;
    if !out.status.success() {
        return Err(format!("{:?} exited {}", command.get_program(), out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The check's derivation: its flake attribute at the commit, as the
/// mirror has it under `refs/proof/HEAD`.
fn derivation(
    cfg: &Config,
    spec: &Spec,
    mirror: &Path,
    attr: &str,
    log: &File,
) -> Result<String, String> {
    let at = mirror
        .to_str()
        .filter(|p| p.bytes().all(crate::plain))
        .ok_or_else(|| format!("{} isn't a plain path", mirror.display()))?;
    if attr.is_empty()
        || !attr
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(format!("{attr:?} isn't a flake attribute"));
    }
    let url = format!(
        "git+file://{at}?ref=refs/proof/{head}&rev={head}#{attr}.drvPath",
        head = spec.head
    );
    let mut c = nix(cfg, "nix");
    c.args([
        "--extra-experimental-features",
        "nix-command flakes",
        "eval",
        "--raw",
        "--option",
        "allow-import-from-derivation",
        "false",
        "--option",
        "accept-flake-config",
        "false",
        "--option",
        "pure-eval",
        "true",
        "--no-update-lock-file",
        &url,
    ]);
    let drv = read(c, log)?;
    if !drv.starts_with('/') || !drv.ends_with(".drv") {
        return Err(format!("{attr} evaluated to {drv:?}, not a derivation"));
    }
    Ok(drv)
}

/// `argv` in the machine's heavy slot, if it has one.
fn heavy(cfg: &Config, argv: &[String]) -> Command {
    let words: Vec<String> = cfg
        .heavy
        .iter()
        .flatten()
        .cloned()
        .chain(argv.iter().cloned())
        .collect();
    let mut c = Command::new(&words[0]);
    c.args(&words[1..]).stdin(Stdio::null());
    c
}

fn outputs(cfg: &Config, drvs: &[String], log: &File) -> Result<Vec<String>, String> {
    let mut c = nix(cfg, "nix-store");
    c.args(["--query", "--outputs"]).args(drvs);
    Ok(read(c, log)?
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

/// Which of `paths` aren't valid in the store.
fn invalid(cfg: &Config, paths: &[String], log: &File) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Ok(vec![]);
    }
    let mut c = nix(cfg, "nix-store");
    c.args(["--check-validity", "--print-invalid"]).args(paths);
    Ok(read(c, log)?
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

/// Ready the derivation: [`PASSED`] when its outputs are all built
/// already, so proving it needs no slot; else its inputs built in a heavy
/// slot, and [`FAILED`], ready for a proof that needs one.
fn prepare(cfg: &Config, drv: &str, log: &File) -> u8 {
    let ready = || -> Result<u8, String> {
        let outs = outputs(cfg, &[drv.to_owned()], log)?;
        if invalid(cfg, &outs, log)?.is_empty() {
            return Ok(PASSED);
        }
        let mut c = nix(cfg, "nix-store");
        c.args(["--query", "--references", drv]);
        let inputs: Vec<String> = read(c, log)?
            .split_whitespace()
            .filter(|p| p.ends_with(".drv"))
            .map(str::to_owned)
            .collect();
        let unbuilt = invalid(cfg, &outputs(cfg, &inputs, log)?, log)?;
        if unbuilt.is_empty() {
            return Ok(FAILED);
        }
        let realise = [
            cfg.nix.join("nix-store").to_string_lossy().into_owned(),
            "--realise".into(),
        ];
        let mut c = heavy(cfg, &[&realise[..], &inputs[..]].concat());
        let (out, err) = (log.try_clone(), log.try_clone());
        let (Ok(out), Ok(err)) = (out, err) else {
            return Err("can't hand its build its log".into());
        };
        let status = c
            .stdout(out)
            .stderr(err)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok(FAILED)
        } else {
            Err(format!("its inputs didn't build: {status}"))
        }
    };
    ready().unwrap_or_else(|why| {
        let _ = writeln!(&mut &*log, "burrow-proof: not ready: {why}");
        BROKE
    })
}

/// nix-store's exits for a build that failed: 100 and up, each bit of the
/// rest a way it did. Anything else is the machine's failure.
const BUILD_FAILED: std::ops::RangeInclusive<i32> = 100..=107;

/// The derivation realised in a heavy slot, its outputs rooted in the
/// run's folder: [`PASSED`] once they're valid, [`FAILED`] on a build
/// failure.
fn realise(cfg: &Config, id: &str, drv: &str, log: &File) -> u8 {
    let root = cfg.out.join(id).join("result");
    let argv = [
        cfg.nix.join("nix-store").to_string_lossy().into_owned(),
        "--realise".into(),
        "--add-root".into(),
        root.to_string_lossy().into_owned(),
        "--indirect".into(),
        drv.into(),
    ];
    let (out, err) = (log.try_clone(), log.try_clone());
    let (Ok(out), Ok(err)) = (out, err) else {
        return BROKE;
    };
    let status = match heavy(cfg, &argv).stdout(out).stderr(err).status() {
        Ok(status) => status,
        Err(e) => {
            let _ = writeln!(&mut &*log, "burrow-proof: can't build: {e}");
            return BROKE;
        }
    };
    match status.code() {
        Some(0) => match outputs(cfg, &[drv.to_owned()], log).and_then(|o| invalid(cfg, &o, log)) {
            Ok(missing) if missing.is_empty() => PASSED,
            Ok(missing) => {
                let _ = writeln!(
                    &mut &*log,
                    "burrow-proof: built, but not in the store: {}",
                    missing.join(", ")
                );
                BROKE
            }
            Err(why) => {
                let _ = writeln!(&mut &*log, "burrow-proof: {why}");
                BROKE
            }
        },
        Some(code) if BUILD_FAILED.contains(&code) => FAILED,
        _ => BROKE,
    }
}

/// The command line in a fresh clone of the commit, in its fence:
/// [`PASSED`] or [`FAILED`] by its exit, and [`BROKE`] when its exit is the
/// machine's.
fn run(
    cfg: &Config,
    id: &str,
    spec: &Spec,
    argv: &[String],
    fence: &mut dyn Fence,
    log: &File,
) -> u8 {
    let note = |text: &str| {
        let _ = writeln!(&mut &*log, "burrow-proof: {text}");
    };
    let Some((program, args)) = argv.split_first() else {
        note("a run with no command proves nothing");
        return BROKE;
    };
    let tree = cfg.trees.join(id);
    let cloned = (|| -> Result<(), String> {
        let _ = fs::remove_dir_all(&tree);
        let mut c = Command::new(&cfg.git);
        c.args(["clone", "-q", "--shared", "--no-checkout"])
            .arg(cfg.mirror(&spec.repo))
            .arg(&tree)
            .stdin(Stdio::null());
        read(c, log)?;
        let mut c = git(cfg, &tree);
        c.args(["checkout", "-q", "--detach", &spec.head]);
        read(c, log).map(drop)
    })();
    if let Err(why) = cloned {
        note(&format!("can't make its clone: {why}"));
        let _ = fs::remove_dir_all(&tree);
        return BROKE;
    }
    note(&format!("{}: {}", spec.check, argv.join(" ")));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let exit = fenced(spec, program, &args, fence, log, &tree);
    // What it left goes before its clone does.
    let ended = match fence.clear(0) {
        Err(e) => {
            note(&format!("can't end what it left: {e}"));
            BROKE
        }
        Ok(()) => match exit.map(|code| (code, verdict(code))) {
            Ok((_, Some(true))) => PASSED,
            Ok((code, Some(false))) => {
                note(&format!("{}: exit {code}", spec.check));
                FAILED
            }
            Ok((code, None)) => {
                note(&format!(
                    "exit {code} is the machine failing, not a verdict"
                ));
                BROKE
            }
            Err(why) => {
                note(&why);
                BROKE
            }
        },
    };
    let _ = fs::remove_dir_all(&tree);
    ended
}

fn fenced(
    spec: &Spec,
    program: &str,
    args: &[&str],
    fence: &mut dyn Fence,
    log: &File,
    tree: &Path,
) -> Result<i32, String> {
    let mut c = fence
        .around(0, Path::new(program), args)
        .map_err(|e| format!("can't make its fence: {e}"))?;
    let (out, err) = (log.try_clone(), log.try_clone());
    let (Ok(out), Ok(err)) = (out, err) else {
        return Err("can't hand it its log".into());
    };
    let status = c
        .current_dir(tree)
        .env("PROOF_REPO", &spec.repo)
        .env("PROOF_HEAD", &spec.head)
        .env("PROOF_CHECK", &spec.check)
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .status()
        .map_err(|e| format!("can't start {program}: {e}"))?;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or_default()))
}
