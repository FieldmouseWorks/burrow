//! What a fix job says it did, in its answers file, held to the problems it
//! was handed before anything of it becomes the plugin's answer: one answer
//! per finding, by its index, in order, each with a reply; a finding fixed
//! only by a push; one outcome. The pushed head is checked against GitHub
//! by the broker.
use conary_core::plugin::{FindingAnswer, FixResult, Problem};
use serde::Deserialize;

/// The answers file, as the brief tells the job to write it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Said {
    /// The commit its push put on the PR's branch, or null.
    pub pushed: Option<String>,
    /// Why it couldn't settle the problems, or null.
    pub unable: Option<String>,
    /// One per finding problem, by its index in the request.
    pub answers: Vec<FindingAnswer>,
}

impl Said {
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes).map_err(|e| format!("the fix job's answers don't read: {e}"))
    }

    /// Its outcome and answers, if they hold for `problems`. A pushed head
    /// is only what the job says: the broker holds it to GitHub's.
    pub fn settle(self, problems: &[Problem]) -> Result<(FixResult, Vec<FindingAnswer>), String> {
        if !crate::findings(problems).eq(self.answers.iter().map(|a| a.problem)) {
            return Err(format!(
                "the fix job's answers are for problems {:?}, not the findings {:?}",
                self.answers.iter().map(|a| a.problem).collect::<Vec<_>>(),
                crate::findings(problems).collect::<Vec<_>>()
            ));
        }
        if let Some(a) = self.answers.iter().find(|a| a.reply.trim().is_empty()) {
            return Err(format!(
                "the fix job's answer to problem {} has no reply",
                a.problem
            ));
        }
        let fixed = self.answers.iter().any(|a| a.fixed);
        let result = match (self.pushed, self.unable) {
            (Some(_), Some(_)) => {
                return Err("the fix job says it both pushed and couldn't".into());
            }
            (Some(head), None) => {
                if !full_sha(&head) {
                    return Err(format!(
                        "the fix job's pushed head {head:?} isn't a full commit id"
                    ));
                }
                FixResult::Pushed { head }
            }
            (None, _) if fixed => {
                return Err("the fix job says it fixed a finding but pushed nothing".into());
            }
            (None, Some(why)) if !why.trim().is_empty() => FixResult::Unable { why },
            (None, Some(_)) => return Err("the fix job says it couldn't, but not why".into()),
            (None, None) => FixResult::Unchanged,
        };
        Ok((result, self.answers))
    }
}

fn full_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}
