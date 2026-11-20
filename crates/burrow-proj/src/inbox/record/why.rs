//! `proj why OWNER/REPO#N`: an issue's record ([`super::Look::record`]),
//! with what GitHub says of it when no job holds it, what the drain's
//! admission says of it when it's ready, and its job's record after it.

use serde_json::Value;

use super::{Record, Stage, Unseen, Wait, files, observer};
use crate::config::Config;
use crate::inbox::drain::Held;
use crate::inbox::{Pass, key};
use crate::job::Job;

/// `target` as OWNER/REPO#N, if it's one: what `proj why` takes for an
/// issue. A job's id never is one, since it names no OWNER/REPO, though it
/// may hold a `#` from its project's name.
pub(crate) fn issue_target(target: &str) -> Option<(&str, &str)> {
    target.rsplit_once('#').filter(|(r, n)| {
        r.split_once('/')
            .is_some_and(|(o, name)| !o.is_empty() && !name.is_empty() && !name.contains('/'))
            && !n.is_empty()
            && n.bytes().all(|b| b.is_ascii_digit())
    })
}

/// `proj why OWNER/REPO#N [--json]`: the issue's record, then its job's
/// when it has one. GitHub is asked about the issue once when no job holds
/// it, since only GitHub can say whether it's closed, or why an issue the
/// inbox keeps nothing of is unseen.
pub(crate) fn why(cfg: &Config, target: &str, json: bool) -> Result<String, crate::Fail> {
    let (repo, n) = issue_target(target)
        .ok_or_else(|| crate::Fail::new(2, "usage: proj why OWNER/REPO#N [--json]"))?;
    let Some(inbox) = cfg.inbox.as_ref() else {
        let r = Record::new(
            target,
            Stage::Unseen(Unseen::Off),
            "nothing: the inbox isn't on here",
        );
        return Ok(says(cfg, &r, json));
    };
    Ok(observer(cfg, inbox).why(repo, n, json))
}

impl Pass<'_> {
    /// [`why`], through this pass.
    pub(in crate::inbox) fn why(&self, repo: &str, n: &str, json: bool) -> String {
        let repo = self
            .project
            .keys()
            .find(|r| r.eq_ignore_ascii_case(repo))
            .cloned()
            .unwrap_or_else(|| repo.to_owned());
        let look = self.look();
        let mut r = look.record(&repo, n, None);
        // Only GitHub can say whether an issue no job holds is closed, or
        // why one the inbox keeps nothing of is unseen: it's asked once.
        // One it refuses to list isn't asked.
        let asks = r.job.is_none()
            && !matches!(
                r.stage,
                Stage::Closed | Stage::Unseen(Unseen::NoProject | Unseen::Unlisted(_))
            );
        let issue = asks
            .then(|| crate::github::api(self.cfg, &format!("repos/{repo}/issues/{n}")))
            .flatten();
        if let Some(issue) = &issue {
            r = look.record(&repo, n, Some(issue));
        }
        // A ready issue the drain hasn't handed on yet: what keeps it, as
        // the drain's own admission reads it.
        let enrolled = files::worker(&self.inbox.state, &key(&repo), n).exists();
        if r.stage == Stage::Ready && r.wait.is_none() && !enrolled {
            let issue = issue.or_else(|| look.listed(&repo, n));
            self.drain_says(&mut r, &repo, issue.as_ref());
        }
        says(self.cfg, &r, json)
    }

    /// What the drain does with a ready issue it hasn't handed on (`i`, as
    /// GitHub or its ready list has it), as [`Pass::admission`] reads its
    /// room: whether the drain is on, its cap with the jobs it holds, a
    /// repair's exemption, and the engine's room.
    fn drain_says(&self, r: &mut Record, repo: &str, i: Option<&Value>) {
        let Some(label) = self.drain_label() else {
            r.next = "nothing hands it on: the drain is off here, so only an engine label put on it starts its job".into();
            return;
        };
        let Some(i) = i else {
            return;
        };
        let going = self.drain_records().len();
        match self.admission(&label, i, repo, going) {
            Ok(engine) if self.lane(i) => {
                r.next = format!(
                    "the drain's next pass hands it on to {}: a repair goes past the drain's cap",
                    engine.name()
                );
            }
            Ok(engine) => {
                r.next = format!(
                    "the drain's next pass hands it on to {}, in its order",
                    engine.name()
                );
            }
            Err(Held::Cap) => {
                r.next = "the drain hands it on once one of its jobs ends, in its order".into();
                *r = r.clone().waiting(Wait::Drain);
            }
            Err(Held::Engine(engine)) => {
                r.next = format!(
                    "the drain hands it on to {} once it has room, in its order",
                    engine.name()
                );
                *r = r.clone().waiting(Wait::Engine(engine.name().to_owned()));
            }
            Err(Held::Unroutable(why)) => {
                r.next =
                    format!("the drain hands it on once an engine can be chosen for it ({why})");
                *r = r.clone().waiting(Wait::Pass);
            }
        }
    }
}

/// The record as `proj why` prints it, followed into its job.
fn says(cfg: &Config, r: &Record, json: bool) -> String {
    let job = r
        .job
        .as_deref()
        .map(|id| (id, Job::new(cfg, id)))
        .filter(|(_, job)| job.dir.is_dir())
        .map(|(id, job)| (id, crate::record::derive(&job)));
    if json {
        let mut v = r.json();
        v["record"] = job.as_ref().map_or(Value::Null, |(id, jr)| jr.json(id));
        return format!("{v}\n");
    }
    let mut out = r.why();
    if let Some((id, jr)) = job {
        out.push_str(&format!("\nIts job, {id}:\n{}", jr.why(id)));
    }
    out
}
