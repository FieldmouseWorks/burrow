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
//! realised in a heavy slot. A `Run` recipe is its command line (a program and its
//! arguments, as the flow's settings give it), in a fresh clone of the
//! commit, in a fence of its own.
use crate::fence::Fence;
use crate::plugin::Bench;
use crate::verdict;
use conary_github::flow::Recipe;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::ops::Deref;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
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

/// The worker's bench, with its run's home, which every command it
/// starts is given.
struct Worker<'a> {
    bench: Bench<'a>,
    home: PathBuf,
}

impl<'a> Deref for Worker<'a> {
    type Target = Bench<'a>;
    fn deref(&self) -> &Bench<'a> {
        &self.bench
    }
}

/// Run `id`'s work on `bench`, each command fenced by `fence`: its exit.
pub fn work(bench: Bench, id: &str, fence: &mut dyn Fence) -> u8 {
    let cfg = Worker {
        bench,
        home: bench.home(id),
    };
    let ended = within(&cfg, id, fence);
    let _ = fs::remove_dir_all(&cfg.home);
    ended
}

fn within(cfg: &Worker, id: &str, fence: &mut dyn Fence) -> u8 {
    // Its log, which the producer made in the run's folder: the worker
    // writes it and can't swap it for anything else.
    let log = match OpenOptions::new().append(true).open(cfg.log(id)) {
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
    let _ = fs::remove_dir_all(&cfg.home);
    if let Err(e) = fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&cfg.home)
    {
        note(&format!("can't make its home: {e}"));
        return BROKE;
    }
    let mirror = cfg.repo(id);
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
        (Recipe::Build(_), _) if cfg.nix.is_none() => {
            note("this worker builds no flake: it has no Nix");
            BROKE
        }
        (Recipe::Build(attr), mode) => match derivation(cfg, &spec, &mirror, attr, &log) {
            Ok(drv) if mode == Mode::Proof => realise(cfg, &drv, &log),
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

/// `program`, in the run's home, with nothing to read.
fn command(cfg: &Worker, program: impl AsRef<std::ffi::OsStr>) -> Command {
    let mut c = Command::new(program);
    c.env("HOME", &cfg.home).stdin(Stdio::null());
    c
}

fn git(cfg: &Worker, dir: &Path) -> Command {
    let mut c = command(cfg, cfg.git);
    c.arg("-C").arg(dir);
    c
}

/// `program` in nix's programs' directory.
fn nix(cfg: &Worker, program: &str) -> Command {
    command(cfg, nix_program(cfg, program))
}

/// `program`'s path in nix's programs' directory, which a worker that
/// builds has (`within` refuses a build to one without).
fn nix_program(cfg: &Worker, program: &str) -> PathBuf {
    cfg.nix.unwrap_or(Path::new("/")).join(program)
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
    cfg: &Worker,
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
fn heavy(cfg: &Worker, argv: &[String]) -> Command {
    let words: Vec<String> = cfg
        .heavy
        .into_iter()
        .flatten()
        .cloned()
        .chain(argv.iter().cloned())
        .collect();
    let mut c = command(cfg, &words[0]);
    c.args(&words[1..]);
    c
}

fn outputs(cfg: &Worker, drvs: &[String], log: &File) -> Result<Vec<String>, String> {
    let mut c = nix(cfg, "nix-store");
    c.args(["--query", "--outputs"]).args(drvs);
    Ok(read(c, log)?
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

/// Which of `paths` aren't valid in the store.
fn invalid(cfg: &Worker, paths: &[String], log: &File) -> Result<Vec<String>, String> {
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
fn prepare(cfg: &Worker, drv: &str, log: &File) -> u8 {
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
            nix_program(cfg, "nix-store").to_string_lossy().into_owned(),
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

/// The derivation realised in a heavy slot: [`PASSED`] once its outputs
/// are valid, [`FAILED`] when a builder in its closure ran and failed.
/// Nothing roots them (the unit's paths are its own, which the daemon
/// can't see), so outputs collected before they're checked are no verdict.
fn realise(cfg: &Worker, drv: &str, log: &File) -> u8 {
    let note = |text: &str| {
        let _ = writeln!(&mut &*log, "burrow-proof: {text}");
    };
    let argv = [
        nix_program(cfg, "nix-store").to_string_lossy().into_owned(),
        "--realise".into(),
        drv.into(),
    ];
    let Ok(out) = log.try_clone() else {
        return BROKE;
    };
    // What it says goes to the log, and is read: through the daemon its
    // exit is 1 however the build failed.
    let said = heavy(cfg, &argv)
        .stdout(out)
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            let mut said = Vec::new();
            if let Some(mut err) = child.stderr.take() {
                err.read_to_end(&mut said)?;
            }
            Ok((child.wait()?, said))
        });
    let (status, said) = match said {
        Ok((status, said)) => (status, String::from_utf8_lossy(&said).into_owned()),
        Err(e) => {
            note(&format!("can't build: {e}"));
            return BROKE;
        }
    };
    let _ = (&*log).write_all(said.as_bytes());
    if status.success() {
        return match outputs(cfg, &[drv.to_owned()], log).and_then(|o| invalid(cfg, &o, log)) {
            Ok(missing) if missing.is_empty() => PASSED,
            Ok(missing) => {
                note(&format!(
                    "built, but not in the store: {}",
                    missing.join(", ")
                ));
                BROKE
            }
            Err(why) => {
                note(&why);
                BROKE
            }
        };
    }
    match failed(&said, |drv| fs::read_to_string(drv)) {
        Ok(true) => FAILED,
        Ok(false) => {
            note(&format!(
                "the build failed with {status}, but no builder did"
            ));
            BROKE
        }
        Err(why) => {
            note(&why);
            BROKE
        }
    }
}

/// Whether what nix said of a failed build is a builder in it failing: one
/// that ran and exited non-zero, and isn't a fixed output's (a fetch,
/// whose failing is the network's), with the disk not full. Anything else
/// is nix or the machine failing, never the check's verdict.
pub(crate) fn failed(
    said: &str,
    read: impl Fn(&str) -> std::io::Result<String>,
) -> Result<bool, String> {
    if said.contains("lack of free disk space") {
        return Ok(false);
    }
    let lines: Vec<&str> = said.lines().map(str::trim).collect();
    for (i, line) in lines.iter().enumerate() {
        let drv = match quoted(line, "Cannot build '") {
            Some(drv)
                if lines.get(i + 1).is_some_and(|next| {
                    next.starts_with("Reason: builder failed with exit code ")
                }) =>
            {
                drv
            }
            _ => continue,
        };
        let text = read(drv).map_err(|e| format!("{drv}: {e}"))?;
        if !fixed(&text).ok_or_else(|| format!("{drv} doesn't read as a derivation"))? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The quoted path in `line` after `before`, when it's a derivation's.
fn quoted<'a>(line: &'a str, before: &str) -> Option<&'a str> {
    let (_, rest) = line.split_once(before)?;
    let (drv, _) = rest.split_once('\'')?;
    (drv.starts_with('/') && drv.ends_with(".drv")).then_some(drv)
}

/// Whether a derivation (its ATerm) has a fixed output: each output is
/// `("NAME","PATH","ALGO","HASH")`, and a fixed one's hash isn't empty.
fn fixed(drv: &str) -> Option<bool> {
    let (outputs, _) = drv.strip_prefix("Derive([")?.split_once("],[")?;
    let mut fixed = false;
    for output in outputs.split("),(") {
        let fields: Vec<&str> = output.trim_matches(['(', ')']).split(',').collect();
        let [_, _, _, hash] = fields[..] else {
            return None;
        };
        fixed |= hash != "\"\"";
    }
    Some(fixed)
}

/// The command line in a fresh clone of the commit, in its fence:
/// [`PASSED`] or [`FAILED`] by its exit, and [`BROKE`] when its exit is the
/// machine's.
fn run(
    cfg: &Worker,
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
        let mut c = command(cfg, cfg.git);
        c.args(["clone", "-q", "--shared", "--no-checkout"])
            .arg(cfg.repo(id))
            .arg(&tree);
        read(c, log)?;
        let mut c = git(cfg, &tree);
        c.args(["checkout", "-q", "--detach", &spec.head]);
        // With no network, an LFS file stays its pointer: it can't be
        // fetched.
        if cfg.offline {
            c.env("GIT_LFS_SKIP_SMUDGE", "1");
        }
        read(c, log).map(drop)
    })();
    if let Err(why) = cloned {
        note(&format!("can't make its clone: {why}"));
        let _ = fs::remove_dir_all(&tree);
        return BROKE;
    }
    note(&format!("{}: {}", spec.check, argv.join(" ")));
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let exit = fenced(cfg, spec, program, &args, fence, log, &tree);
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
    cfg: &Worker,
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
        .env("HOME", &cfg.home)
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
