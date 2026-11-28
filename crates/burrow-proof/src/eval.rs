//! `burrow-proof eval-start|eval-run|eval-work|eval-end DIR`: a proof
//! derivation evaluation, run in a transient user systemd unit so the
//! steward's pass never blocks on a multi-minute `nix eval`.
//!
//! - `eval-start DIR` reads `DIR/build.json` ([`EvalRecord`]) and starts
//!   its unit via `systemd-run --user` with `RuntimeMaxSec` and
//!   `ExecStopPost` set to `burrow-proof eval-end DIR`.
//! - `eval-run DIR` enters `heavy --wait ... --` (when configured) and invokes
//!   `eval-work DIR`.
//! - `eval-work DIR` evaluates each check's `.drvPath` and queries its
//!   outputs (computing a speculative `git merge-tree` first for
//!   [`EvalKind::Merged`]), writing `DIR/ran.json`.
//! - `eval-end DIR` writes `DIR/end.json` and kicks the repo's pass.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::end::{Ended, kick};
use crate::{END, LOG, RAN, plain, save};

/// Which derivation evaluation a unit performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalKind {
    Outputs,
    Merged,
}

/// What an eval unit runs, as the steward writes it in `DIR/build.json`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvalRecord {
    pub repo: String,
    pub number: u64,
    pub head: String,
    pub kind: EvalKind,
    pub mirror: PathBuf,
    pub attrs: BTreeMap<String, String>,
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub tip: Option<String>,
    pub unit: String,
    pub timeout: u64,
    pub nix: PathBuf,
    pub git: PathBuf,
    #[serde(default)]
    pub heavy: Option<PathBuf>,
    pub kick: Option<String>,
    /// Marks the build folder as short-lived (same retention as prebuilds).
    #[serde(default)]
    pub eval: bool,
}

/// Intermediate result written by `eval-work` to `DIR/ran.json`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct EvalRan {
    pub drvs: Option<BTreeMap<String, String>>,
    pub paths: Option<Vec<String>>,
    pub failed: bool,
    #[serde(default)]
    pub timed_out: bool,
}

/// Maximum seconds an eval unit waits in `heavy`'s queue before giving up,
/// kept separate from `rec.timeout` so queue wait time doesn't eat eval time.
const QUEUE_BACKSTOP_SECS: u64 = 10800;

/// Grace added on top of `QUEUE_BACKSTOP_SECS + rec.timeout` for the outer
/// systemd `RuntimeMaxSec` backstop so `heavy` handoff and `eval-work`'s own
/// deadline cleanup finish before systemd kills the unit.
const BACKSTOP_GRACE_SECS: u64 = 60;

/// Exit status `heavy` returns when `--wait` expires before a slot frees
/// (`burrow_heavy::BUSY`).
const HEAVY_BUSY: i32 = 75;

/// Output cap per `git` / `nix` call in `eval-work`.
const MAX_OUTPUT: usize = 1024 * 1024;

impl EvalRecord {
    pub fn read(dir: &Path) -> Result<Self, String> {
        let path = dir.join("build.json");
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let rec: Self =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        rec.check()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(rec)
    }

    fn check(&self) -> Result<(), String> {
        if self.repo.split('/').count() != 2 || self.repo.split('/').any(str::is_empty) {
            return Err(format!("repo {:?} isn't OWNER/REPO", self.repo));
        }
        if self.number == 0 {
            return Err("number 0 isn't a PR's".into());
        }
        if !is_sha(&self.head) {
            return Err(format!("head {:?} isn't a commit's full hash", self.head));
        }
        if self.attrs.is_empty() {
            return Err("no attrs".into());
        }
        for (check, attr) in &self.attrs {
            if !is_attr(attr) {
                return Err(format!("{check}: {attr:?} isn't a flake attribute"));
            }
        }
        for (name, p) in [
            ("mirror", &self.mirror),
            ("nix", &self.nix),
            ("git", &self.git),
        ] {
            if !p.is_absolute() {
                return Err(format!("{name} {} isn't an absolute path", p.display()));
            }
        }
        if let Some(heavy) = &self.heavy
            && !heavy.is_absolute()
        {
            return Err(format!("heavy {} isn't an absolute path", heavy.display()));
        }
        if self.kind == EvalKind::Merged {
            if self.base.as_deref().is_none_or(str::is_empty) {
                return Err("merged eval requires base".into());
            }
            if !self.tip.as_deref().is_some_and(is_sha) {
                return Err(format!("tip {:?} isn't a commit's full hash", self.tip));
            }
        }
        let unit = self
            .unit
            .strip_prefix("autoland-")
            .and_then(|u| u.strip_suffix(".service"));
        if !unit.is_some_and(|u| !u.is_empty() && u.bytes().all(plain)) {
            return Err(format!("unit {:?} isn't autoland-ID.service", self.unit));
        }
        if self.timeout == 0 {
            return Err("timeout 0".into());
        }
        Ok(())
    }
}

fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_attr(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn word(p: &Path) -> Result<String, String> {
    p.to_str()
        .filter(|s| p.is_absolute() && s.bytes().all(plain))
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "{} isn't a path a unit's command line takes as it is",
                p.display()
            )
        })
}

/// `systemd-run` arguments for an eval unit whose folder is `dir`.
pub fn argv(exe: &str, dir: &str, rec: &EvalRecord) -> Vec<String> {
    let runtime_max = if rec.heavy.is_some() {
        QUEUE_BACKSTOP_SECS
            .saturating_add(rec.timeout)
            .saturating_add(BACKSTOP_GRACE_SECS)
    } else {
        rec.timeout
    };
    vec![
        "--user".into(),
        "--quiet".into(),
        "--collect".into(),
        format!("--unit={}", rec.unit),
        format!("--property=RuntimeMaxSec={}s", runtime_max),
        format!("--property=ExecStopPost=:{exe} eval-end {dir}"),
        "--expand-environment=no".into(),
        "--".into(),
        exe.into(),
        "eval-run".into(),
        dir.into(),
    ]
}

/// Start the eval unit for `dir` via `systemd_run`.
pub fn start(dir: &Path, exe: &Path, systemd_run: &Path) -> Result<(), String> {
    let rec = EvalRecord::read(dir)?;
    let (exe, dir) = (word(exe)?, word(dir)?);
    let mut c = Command::new(systemd_run);
    c.args(argv(&exe, &dir, &rec)).stdin(Stdio::null());
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        let uid = fs::metadata("/proc/self")
            .map_err(|e| format!("/proc/self: {e}"))?
            .uid();
        c.env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
    }
    let out = c
        .output()
        .map_err(|e| format!("{}: {e}", systemd_run.display()))?;
    if out.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&out.stderr);
    Err(format!("systemd-run refused {}: {}", rec.unit, said.trim()))
}

/// Unit's main process: enter `heavy` when configured, then run `eval-work`.
pub fn run(dir: &Path, exe: &Path) -> Result<(), String> {
    let rec = EvalRecord::read(dir)?;
    let Some(heavy) = &rec.heavy else {
        return work(dir);
    };
    let wait = QUEUE_BACKSTOP_SECS.to_string();
    let status = Command::new(heavy)
        .args(["--wait", &wait, "--"])
        .arg(exe)
        .arg("eval-work")
        .arg(dir)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("{}: {e}", heavy.display()))?;
    if status.success() {
        Ok(())
    } else if status.code() == Some(HEAVY_BUSY) {
        let why = format!("heavy timed out waiting for a slot after {QUEUE_BACKSTOP_SECS}s");
        if let Ok(log) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(LOG))
        {
            note(&log, &why);
        }
        let ran = EvalRan {
            drvs: None,
            paths: None,
            failed: true,
            timed_out: true,
        };
        let _ = save(&dir.join(RAN), &ran);
        Err(why)
    } else {
        Err(format!("heavy exited {status}"))
    }
}

/// Do the actual git/nix evaluation and write `DIR/ran.json`.
pub fn work(dir: &Path) -> Result<(), String> {
    let rec = EvalRecord::read(dir)?;
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG))
        .map_err(|e| format!("{}: {e}", dir.join(LOG).display()))?;
    accepted(dir)?;
    let deadline = Instant::now() + Duration::from_secs(rec.timeout);
    let ran = match rec.kind {
        EvalKind::Outputs => {
            let ref_name = format!("refs/pr/{}", rec.number);
            eval_derivations(&rec, &ref_name, &rec.head, deadline, &log)
        }
        EvalKind::Merged => eval_merged(&rec, deadline, &log),
    };
    match ran {
        Ok(ran) => {
            save(&dir.join(RAN), &ran).map_err(|e| format!("{}: {e}", dir.join(RAN).display()))
        }
        Err(why) => {
            if Instant::now() >= deadline {
                let ran = EvalRan {
                    drvs: None,
                    paths: None,
                    failed: true,
                    timed_out: true,
                };
                let _ = save(&dir.join(RAN), &ran);
            }
            Err(why)
        }
    }
}

fn run_bounded(
    cmd: &mut Command,
    deadline: Instant,
    log: &File,
    what: &str,
) -> Result<Output, String> {
    burrow_process::output(cmd, None, deadline, MAX_OUTPUT).map_err(|e| {
        note(log, &format!("{what} failed: {e}"));
        format!("{what}: {e}")
    })
}

fn eval_merged(rec: &EvalRecord, deadline: Instant, log: &File) -> Result<EvalRan, String> {
    let tip = rec.tip.as_deref().ok_or("missing tip")?;
    let base = rec.base.as_deref().ok_or("missing base")?;
    let mut up_cmd = Command::new(&rec.git);
    up_cmd
        .arg("-C")
        .arg(&rec.mirror)
        .args(["merge-base", "--is-ancestor", tip, &rec.head]);
    let up = run_bounded(&mut up_cmd, deadline, log, "git merge-base")?;
    match up.status.code() {
        Some(0) => {
            return Ok(EvalRan {
                drvs: None,
                paths: None,
                failed: false,
                timed_out: false,
            });
        }
        Some(1) => {}
        _ => {
            let err = String::from_utf8_lossy(&up.stderr);
            note(log, &format!("git merge-base failed: {}", err.trim()));
            return Err(format!("git merge-base: {}", tail(err.trim())));
        }
    }
    let mut tree_cmd = Command::new(&rec.git);
    tree_cmd.arg("-C").arg(&rec.mirror).args([
        "merge-tree",
        "--write-tree",
        "--no-messages",
        tip,
        &rec.head,
    ]);
    let tree = run_bounded(&mut tree_cmd, deadline, log, "git merge-tree")?;
    match tree.status.code() {
        Some(1) => {
            return Ok(EvalRan {
                drvs: None,
                paths: None,
                failed: false,
                timed_out: false,
            });
        }
        Some(0) => {}
        _ => {
            let err = String::from_utf8_lossy(&tree.stderr);
            note(log, &format!("git merge-tree failed: {}", err.trim()));
            return Err(format!("git merge-tree: {}", tail(err.trim())));
        }
    }
    let tree_oid = String::from_utf8_lossy(&tree.stdout)
        .split_whitespace()
        .next()
        .ok_or("git merge-tree produced no tree")?
        .to_owned();
    let msg = format!("#{} brought up to date with {base}", rec.number);
    let rev = git_stdout(
        &rec.git,
        &rec.mirror,
        &[
            "-c",
            "user.name=autoland",
            "-c",
            "user.email=autoland@localhost",
            "-c",
            "commit.gpgsign=false",
            "commit-tree",
            &tree_oid,
            "-p",
            &rec.head,
            "-p",
            tip,
            "-m",
            &msg,
        ],
        deadline,
        log,
    )?;
    let ref_name = format!("refs/merged/{}", rec.number);
    git_stdout(
        &rec.git,
        &rec.mirror,
        &["update-ref", &ref_name, &rev],
        deadline,
        log,
    )?;
    eval_derivations(rec, &ref_name, &rev, deadline, log)
}

fn eval_derivations(
    rec: &EvalRecord,
    ref_name: &str,
    rev: &str,
    deadline: Instant,
    log: &File,
) -> Result<EvalRan, String> {
    let mirror = rec
        .mirror
        .to_str()
        .ok_or("mirror path is not valid UTF-8")?;
    let url = format!("git+file://{}?ref={ref_name}&rev={rev}", quote_path(mirror));
    let mut drvs = BTreeMap::new();
    for (check, attr) in &rec.attrs {
        let target = format!("{url}#{attr}.drvPath");
        let drv = run_nix(
            &rec.nix,
            "nix",
            &[
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
                "--no-update-lock-file",
                &target,
            ],
            deadline,
            log,
        )?;
        drvs.insert(check.clone(), drv);
    }
    let mut query_args = vec!["--query", "--outputs"];
    let drv_list: Vec<&str> = drvs.values().map(String::as_str).collect();
    query_args.extend(drv_list);
    let out = run_nix(&rec.nix, "nix-store", &query_args, deadline, log)?;
    let paths: Vec<String> = out
        .split_whitespace()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if paths.is_empty() {
        Ok(EvalRan {
            drvs: None,
            paths: None,
            failed: false,
            timed_out: false,
        })
    } else {
        Ok(EvalRan {
            drvs: Some(drvs),
            paths: Some(paths),
            failed: false,
            timed_out: false,
        })
    }
}

fn git_stdout(
    git: &Path,
    mirror: &Path,
    args: &[&str],
    deadline: Instant,
    log: &File,
) -> Result<String, String> {
    let mut cmd = Command::new(git);
    cmd.arg("-C").arg(mirror).args(args);
    let out = run_bounded(&mut cmd, deadline, log, "git")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        note(log, &format!("git {:?} failed: {}", args, err.trim()));
        return Err(format!("git: {}", tail(err.trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn run_nix(
    nix_dir: &Path,
    tool: &str,
    args: &[&str],
    deadline: Instant,
    log: &File,
) -> Result<String, String> {
    let mut cmd = Command::new(nix_dir.join(tool));
    cmd.args(args);
    let out = run_bounded(&mut cmd, deadline, log, tool)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        note(log, &format!("{tool} failed: {}", err.trim()));
        return Err(format!("{tool}: {}", tail(err.trim())));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn quote_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn tail(s: &str) -> &str {
    let keep = s.len().saturating_sub(300);
    let mut idx = keep;
    while idx < s.len() && !s.is_char_boundary(idx) {
        idx += 1;
    }
    &s[idx..]
}

fn note(mut log: &File, text: &str) {
    let _ = writeln!(log, "autoland: {text}");
}

fn accepted(dir: &Path) -> Result<(), String> {
    match fs::remove_file(dir.join("policy-restoring")) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

/// Unit's `ExecStopPost`: write `DIR/end.json` and kick the repo's pass.
pub fn end(dir: &Path, how: &Ended) -> Result<Value, String> {
    if dir.join("policy-paused").exists() || dir.join("policy-restoring").exists() {
        let value = json!({"state": "parked"});
        save(&dir.join(END), &value).map_err(|e| e.to_string())?;
        return Ok(value);
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(LOG));
    let log_note = |text: &str| {
        if let Ok(log) = log.as_ref() {
            note(log, text);
        }
        eprintln!("burrow-proof: {}: {text}", dir.display());
    };
    let rec = EvalRecord::read(dir);
    let ran: Option<EvalRan> = fs::read_to_string(dir.join(RAN))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();
    let exit: Option<i32> = (how.code == "exited")
        .then(|| how.status.parse().ok())
        .flatten();
    let said = match ran {
        Some(ran) if ran.timed_out || how.result == "timeout" => json!({
            "state": "timed out",
            "exit": null,
            "drvs": null,
            "paths": null,
            "failed": true,
            "at": at,
        }),
        Some(ran) if how.result == "success" && exit == Some(0) => json!({
            "state": "done",
            "exit": 0,
            "drvs": ran.drvs,
            "paths": ran.paths,
            "failed": ran.failed,
            "at": at,
        }),
        _ if how.result == "timeout" => json!({
            "state": "timed out",
            "exit": null,
            "drvs": null,
            "paths": null,
            "failed": true,
            "at": at,
        }),
        _ => json!({
            "state": "failed",
            "exit": exit,
            "drvs": null,
            "paths": null,
            "failed": true,
            "at": at,
        }),
    };
    log_note(&format!(
        "eval ended {} ({} {} {})",
        said["state"].as_str().unwrap_or_default(),
        how.result,
        how.code,
        how.status
    ));
    save(&dir.join(END), &said).map_err(|e| format!("{}: {e}", dir.join(END).display()))?;
    if let Ok(Some(url)) = rec.as_ref().map(|r| r.kick.as_deref())
        && let Err(e) = kick(url)
    {
        log_note(&format!("couldn't kick {url}: {e}"));
    }
    Ok(said)
}
