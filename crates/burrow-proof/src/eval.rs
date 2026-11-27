//! `burrow-proof eval-start|eval-run|eval-work|eval-end DIR`: a proof
//! derivation evaluation, run in a transient user systemd unit so the
//! steward's pass never blocks on a multi-minute `nix eval`.
//!
//! - `eval-start DIR` reads `DIR/build.json` ([`EvalRecord`]) and starts
//!   its unit via `systemd-run --user` with `RuntimeMaxSec` and
//!   `ExecStopPost` set to `burrow-proof eval-end DIR`.
//! - `eval-run DIR` enters `heavy --` (when configured) and invokes
//!   `eval-work DIR`.
//! - `eval-work DIR` evaluates each check's `.drvPath` and queries its
//!   outputs (computing a speculative `git merge-tree` first for
//!   [`EvalKind::Merged`]), writing `DIR/ran.json`.
//! - `eval-end DIR` writes `DIR/end.json` and kicks the repo's pass.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::end::{Ended, kick};
use crate::{END, LOG, RAN, plain, save};

const TIMED_OUT: &str = "timed-out";
/// Extra wall-clock budget for waiting in `heavy`'s queue before `eval-work`
/// starts, so a busy machine doesn't kill queued evals at `rec.timeout` while
/// still bounding a permanently wedged queue.
const QUEUE_CAP: u64 = 7200;

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
}

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
///
/// When `heavy` is configured, `eval-run` may wait in `heavy`'s queue before
/// `eval-work` starts, so `RuntimeMaxSec` includes [`QUEUE_CAP`] as a generous
/// outer bound while `eval-work` enforces `rec.timeout` once in the slot.
pub fn argv(exe: &str, dir: &str, rec: &EvalRecord) -> Vec<String> {
    let runtime_max = if rec.heavy.is_some() {
        rec.timeout.saturating_add(QUEUE_CAP)
    } else {
        rec.timeout
    };
    vec![
        "--user".into(),
        "--quiet".into(),
        "--collect".into(),
        format!("--unit={}", rec.unit),
        format!("--property=RuntimeMaxSec={runtime_max}s"),
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
    let status = Command::new(heavy)
        .args(["--"])
        .arg(exe)
        .arg("eval-work")
        .arg(dir)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("{}: {e}", heavy.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("heavy exited {status}"))
    }
}

enum CmdError {
    TimedOut,
    Failed(String),
}

impl From<String> for CmdError {
    fn from(s: String) -> Self {
        Self::Failed(s)
    }
}

/// Do the actual git/nix evaluation and write `DIR/ran.json`, bounding the work
/// itself to `rec.timeout` seconds.
pub fn work(dir: &Path) -> Result<(), String> {
    let rec = EvalRecord::read(dir)?;
    let _ = fs::remove_file(dir.join(TIMED_OUT));
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
    let ran = match ran {
        Ok(ran) => ran,
        Err(CmdError::TimedOut) => {
            let _ = fs::write(dir.join(TIMED_OUT), b"timed out\n");
            note(&log, &format!("eval timed out after {}s", rec.timeout));
            return Err(format!("timed out after {}s", rec.timeout));
        }
        Err(CmdError::Failed(why)) => return Err(why),
    };
    save(&dir.join(RAN), &ran).map_err(|e| format!("{}: {e}", dir.join(RAN).display()))
}

fn eval_merged(rec: &EvalRecord, deadline: Instant, log: &File) -> Result<EvalRan, CmdError> {
    let tip = rec.tip.as_deref().ok_or("missing tip".to_owned())?;
    let base = rec.base.as_deref().ok_or("missing base".to_owned())?;
    let mut cmd = Command::new(&rec.git);
    cmd.arg("-C")
        .arg(&rec.mirror)
        .args(["merge-base", "--is-ancestor", tip, &rec.head]);
    let up = run_until(cmd, "git merge-base", deadline)?;
    match up.status.code() {
        Some(0) => {
            return Ok(EvalRan {
                drvs: None,
                paths: None,
                failed: false,
            });
        }
        Some(1) => {}
        _ => {
            let err = String::from_utf8_lossy(&up.stderr);
            note(log, &format!("git merge-base failed: {}", err.trim()));
            return Err(CmdError::Failed(format!(
                "git merge-base: {}",
                tail(err.trim())
            )));
        }
    }
    let mut cmd = Command::new(&rec.git);
    cmd.arg("-C").arg(&rec.mirror).args([
        "merge-tree",
        "--write-tree",
        "--no-messages",
        tip,
        &rec.head,
    ]);
    let tree = run_until(cmd, "git merge-tree", deadline)?;
    match tree.status.code() {
        Some(1) => {
            return Ok(EvalRan {
                drvs: None,
                paths: None,
                failed: false,
            });
        }
        Some(0) => {}
        _ => {
            let err = String::from_utf8_lossy(&tree.stderr);
            note(log, &format!("git merge-tree failed: {}", err.trim()));
            return Err(CmdError::Failed(format!(
                "git merge-tree: {}",
                tail(err.trim())
            )));
        }
    }
    let tree_oid = String::from_utf8_lossy(&tree.stdout)
        .split_whitespace()
        .next()
        .ok_or("git merge-tree produced no tree".to_owned())?
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
) -> Result<EvalRan, CmdError> {
    let mirror = rec
        .mirror
        .to_str()
        .ok_or("mirror path is not valid UTF-8".to_owned())?;
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
        })
    } else {
        Ok(EvalRan {
            drvs: Some(drvs),
            paths: Some(paths),
            failed: false,
        })
    }
}

fn git_stdout(
    git: &Path,
    mirror: &Path,
    args: &[&str],
    deadline: Instant,
    log: &File,
) -> Result<String, CmdError> {
    let mut cmd = Command::new(git);
    cmd.arg("-C").arg(mirror).args(args);
    let out = run_until(cmd, "git", deadline)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        note(log, &format!("git {:?} failed: {}", args, err.trim()));
        return Err(CmdError::Failed(format!("git: {}", tail(err.trim()))));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn run_nix(
    nix_dir: &Path,
    tool: &str,
    args: &[&str],
    deadline: Instant,
    log: &File,
) -> Result<String, CmdError> {
    let mut cmd = Command::new(nix_dir.join(tool));
    cmd.args(args);
    let out = run_until(cmd, tool, deadline)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        note(log, &format!("{tool} failed: {}", err.trim()));
        return Err(CmdError::Failed(format!("{tool}: {}", tail(err.trim()))));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn run_until(mut cmd: Command, label: &str, deadline: Instant) -> Result<Output, CmdError> {
    if Instant::now() >= deadline {
        return Err(CmdError::TimedOut);
    }
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| CmdError::Failed(format!("{label}: {e}")))?;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut p) = out_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(ref mut p) = err_pipe {
            let _ = p.read_to_end(&mut buf);
        }
        buf
    });
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = out_reader.join().unwrap_or_default();
                let stderr = err_reader.join().unwrap_or_default();
                return Ok(Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) => {
                let now = Instant::now();
                if now >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CmdError::TimedOut);
                }
                thread::sleep(Duration::from_millis(25).min(deadline - now));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CmdError::Failed(format!("{label}: {e}")));
            }
        }
    }
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
        Some(ran) if how.result == "success" && exit == Some(0) => json!({
            "state": "done",
            "exit": 0,
            "drvs": ran.drvs,
            "paths": ran.paths,
            "failed": ran.failed,
            "at": at,
        }),
        _ if how.result == "timeout" || dir.join(TIMED_OUT).exists() => json!({
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
