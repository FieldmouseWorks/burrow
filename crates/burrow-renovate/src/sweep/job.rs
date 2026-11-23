//! Which job a PR was handed to, and whether that job is still at work: a
//! PR whose job is queued, running or holds something open (an await, a
//! question) goes to no other, whatever has changed on it since, as two
//! jobs on one branch can only duplicate each other's work or race each
//! other's pushes (burrow#1194).
//!
//! Each handover is named by a proj key of its own (`proj run --key`),
//! kept beside each PR's key (`KEY.job`) before the job starts, so a sweep
//! that dies while proj starts it still knows the job; one that never
//! started is none (`proj find` lets its key go).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use burrow_process::lifecycle::Lifecycle;

use super::{Broke, Config, Handed, steward, task, with_suffix, write};
use crate::{complain, say};

/// Where PR `key`'s handover is kept: the proj key of the job it was last
/// handed to.
pub(super) fn path(key: &Path) -> PathBuf {
    with_suffix(key, ".job")
}

/// A new handover's proj key, one no other handover has: proj holds a key
/// for good once its job has started. A repo's name may hold `_`, which a
/// key can't.
pub(super) fn key(repo: &str) -> String {
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        "renovate-sweep/{}/{at}-{}",
        repo.replace('_', "."),
        std::process::id()
    )
}

impl Config {
    /// The job PR `key` was last handed to, if it's still at work
    /// (Lifecycle::at_work). One proj can't say about breaks the pass:
    /// taken for ended, it could get a second job; taken for at work
    /// quietly, the PR would wait on it for good.
    pub(super) fn at_work(&self, key: &Path) -> Result<Option<String>, Broke> {
        let path = path(key);
        let handover = match fs::read_to_string(&path) {
            Ok(text) => text.trim().to_owned(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(Broke(format!("{}: {e}", path.display()))),
        };
        if handover.is_empty() {
            return Ok(None);
        }
        let proj = &self.tools.proj;
        let out = Command::new(proj)
            .args(["find", &handover])
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Broke(format!("{}: {e}", proj.display())))?;
        if !out.status.success() {
            return Err(Broke(format!(
                "proj find {handover}: {} {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        // Never started: nothing to wait for.
        if id.is_empty() {
            return Ok(None);
        }
        let life = Lifecycle::read(proj, &id);
        if life.state.is_none() {
            return Err(Broke(format!(
                "proj lifecycle {id} can't say whether it's at work"
            )));
        }
        Ok(life.at_work(None).then_some(id))
    }

    /// `handed` to one Claude job in `project`.
    pub(super) fn hand(
        &self,
        project: &str,
        repo: &str,
        info: &steward::Repo,
        handed: &[Handed],
        handover: &str,
    ) -> Result<(), Broke> {
        let mut seals = String::new();
        for h in handed {
            if let Some(m) = &h.major {
                let [ok, held] = self.seal(repo, m)?;
                seals.push_str(&format!(
                    "- #{} on {} for {}: ok `{ok}`, held `{held}`\n",
                    m.n, m.sha, m.base
                ));
            }
        }
        let todo: String = handed.iter().map(|h| h.line.as_str()).collect();
        let task = task::task(
            self,
            repo,
            todo.trim_end_matches('\n'),
            &info.proofs,
            seals.trim_end_matches('\n'),
        );
        let mut proj = Command::new(&self.tools.proj);
        // The kind of work it is, which the machine may give a model of its own.
        proj.args(["run", "--pipeline", "--role", "renovate-sweep"]);
        if let Some(app) = &self.app {
            proj.args(["--app", &app.name]);
        }
        // Named, so the next pass can tell whether it's still at work, and
        // that pass comes as each of its turns ends.
        proj.args([
            "--key",
            handover,
            "--kick",
            &format!("renovate-sweep/{repo}"),
        ]);
        let ran = proj
            .args([project, &task])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status();
        let why = match ran {
            Ok(s) if s.success() => {
                say(&format!(
                    "{repo}: {} PRs handed to one Claude job",
                    todo.matches('\n').count()
                ));
                return Ok(());
            }
            Ok(s) => format!("{repo}: proj run: {s}"),
            Err(e) => format!("{}: {e}", self.tools.proj.display()),
        };
        Err(Broke(why))
    }
}

/// Each of `files` as it was `before` (none: not there).
pub(super) fn put_back(name: &str, files: &[PathBuf], before: [Option<String>; 2]) {
    for (f, before) in files.iter().zip(before) {
        let back = match before {
            Some(text) => write(f, &text),
            None => match fs::remove_file(f) {
                Err(e) if e.kind() != io::ErrorKind::NotFound => {
                    Err(Broke(format!("{}: {e}", f.display())))
                }
                _ => Ok(()),
            },
        };
        if let Err(Broke(why)) = back {
            complain(&format!("{name}: {why}"));
        }
    }
}
