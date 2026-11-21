//! How a Claude job stands and what it holds open, as `proj lifecycle ID`
//! says it: the one word every door that hands work to a job reads on
//! whether that job is still at work.
use std::path::Path;
use std::process::{Command, Stdio};

/// How a Claude job stands and what it holds open, read together (`proj
/// lifecycle`, burrow-proj's Job::lifecycle, under the job's turn-start
/// lock), so no turn starts between the two.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lifecycle {
    /// queued, running, or how its latest turn ended (done, failed, timed
    /// out ...); None when nothing can say.
    pub state: Option<String>,
    /// What it holds open, a line each: `ask`, `owner`, `await TARGET`.
    pub holds: Vec<String>,
}

impl Lifecycle {
    /// What `proj` (the program at `proj`) says of job `jid`. Nothing can
    /// say (state None) when proj can't: it couldn't read the job's
    /// record, or isn't there to ask.
    pub fn read(proj: &Path, jid: &str) -> Lifecycle {
        let Ok(out) = Command::new(proj)
            .args(["lifecycle", jid])
            .stdin(Stdio::null())
            .output()
        else {
            return Lifecycle::default();
        };
        if !out.status.success() {
            return Lifecycle::default();
        }
        Lifecycle::parse(&String::from_utf8_lossy(&out.stdout))
    }

    /// `proj lifecycle`'s lines: its state, then a line for each hold.
    pub fn parse(said: &str) -> Lifecycle {
        let mut lines = said.lines().map(str::trim);
        match lines.next().filter(|s| !s.is_empty()) {
            Some(state) => Lifecycle {
                state: Some(state.into()),
                holds: lines.map(String::from).collect(),
            },
            None => Lifecycle::default(),
        }
    }

    /// Whether the job is still at work: queued, running, or nothing can
    /// say; or, however its turn ended, it holds something open, a question
    /// for a person or a PR or issue it awaits other than `own` (the PR it
    /// was handed: awaiting that is ending its turn), which proj resumes it
    /// on. The one word on whether a job ended, for every kind (the
    /// steward's Python reads the same, common.job_lifecycle): one that
    /// holds a question taken for ended gets a second job on the same work,
    /// and both carry on once the question's answered or times out
    /// (remi#881).
    pub fn at_work(&self, own: Option<&str>) -> bool {
        matches!(
            self.state.as_deref(),
            None | Some("queued" | "running" | "parked")
        ) || self.holds.iter().any(|h| match h.trim().split_once(' ') {
            Some(("await", target)) => own.is_none_or(|o| !target.trim().eq_ignore_ascii_case(o)),
            _ => !h.trim().is_empty(),
        })
    }

    /// Whether what it holds open includes `own`, the PR it was handed:
    /// its turn is over, but proj resumes it on that PR's next change. A
    /// fix job's claim follows it from turn to turn so (claims.rs).
    pub fn awaits(&self, own: &str) -> bool {
        self.holds.iter().any(|h| {
            matches!(h.trim().split_once(' '),
                Some(("await", target)) if target.trim().eq_ignore_ascii_case(own))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::Lifecycle;

    #[test]
    fn its_lines_are_a_state_then_holds() {
        let l = Lifecycle::parse("done\nawait https://github.com/sam/r/pull/1\n");
        assert_eq!(l.state.as_deref(), Some("done"));
        assert!(l.at_work(None));
        assert!(!l.at_work(Some("https://github.com/sam/r/pull/1")));
        assert!(!Lifecycle::parse("done\n").at_work(None));
        assert!(Lifecycle::parse("running\n").at_work(None));
        assert!(Lifecycle::parse("done\nask\n").at_work(None));
        // Nothing said is nothing known: at work.
        assert_eq!(Lifecycle::parse(""), Lifecycle::default());
        assert!(Lifecycle::default().at_work(None));
    }
}
