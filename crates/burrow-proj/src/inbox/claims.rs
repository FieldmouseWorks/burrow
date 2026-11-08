//! References in the brief of an inbox job belong to that job while it runs.

use std::collections::BTreeSet;
use std::fs;

use super::{Pass, dirs};
use crate::job::Job;
use crate::read;

impl Pass<'_> {
    /// Issue numbers handed to running or queued inbox jobs in this repo.
    /// Read the handed snapshot, never newer text that the job hasn't seen.
    pub(super) fn claimed(&self, repo: &str) -> BTreeSet<String> {
        let mut claimed = BTreeSet::new();
        for d in dirs(&self.inbox.state, "") {
            let issue = read(&d.join("issue")).unwrap_or_default();
            let Some((of, _)) = issue.rsplit_once('#') else {
                continue;
            };
            if !of.eq_ignore_ascii_case(repo) {
                continue;
            }
            let Some(id) = read(&d.join("job")) else {
                continue;
            };
            let job = Job::new(self.cfg, &id);
            if !job.dir.is_dir() || !matches!(job.state().as_str(), "queued" | "running") {
                continue;
            }
            let input = fs::read_to_string(d.join("input")).unwrap_or_default();
            for (at, _) in input.match_indices('#') {
                // A bare #N is local; OWNER/REPO#N must name this repo.
                let qualifier = input[..at]
                    .rsplit(|c: char| !c.is_ascii_alphanumeric() && !"/_.-".contains(c))
                    .next()
                    .unwrap_or_default();
                // Slashes also separate bare references, as in #7/#8.
                if !qualifier.is_empty()
                    && !qualifier.ends_with('/')
                    && !qualifier.eq_ignore_ascii_case(repo)
                {
                    continue;
                }
                let digits: String = input[at + 1..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if let Ok(n) = digits.parse::<u64>()
                    && n > 0
                {
                    claimed.insert(n.to_string());
                }
            }
        }
        claimed
    }
}
