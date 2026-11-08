//! Only a complete, successful listing may replace a repo's deadlines.
use super::{Broke, Config};
use serde_json::Value;
use std::process::{Command, Stdio};

impl Config {
    pub(super) fn list(&self, repo: &str) -> Result<Vec<Value>, Broke> {
        let fail = |why| Broke(format!("{}: {repo}: gh pr list: {why}", self.name));
        let out = Command::new(&self.tools.gh)
            .args(["pr", "list", "-R", repo, "--state", "open", "--search", "head:renovate/", "--limit", "1000", "--json",
                "number,title,headRefName,headRefOid,baseRefName,labels,createdAt,statusCheckRollup"])
            .stdin(Stdio::null()).stderr(Stdio::inherit()).output()
            .map_err(|e| fail(e.to_string()))?;
        if !out.status.success() {
            return Err(fail(out.status.to_string()));
        }
        let prs: Vec<Value> =
            serde_json::from_slice(&out.stdout).map_err(|e| fail(e.to_string()))?;
        // Reaching the listing cap cannot establish that every PR was seen.
        if prs.len() >= 1000
            || prs.iter().any(|pr| {
                !pr["number"].is_u64()
                    || ["title", "headRefName", "headRefOid", "baseRefName"]
                        .iter()
                        .any(|key| !pr[key].is_string())
                    || !pr["labels"].is_array()
                    || !pr["statusCheckRollup"].is_array()
            })
        {
            return Err(fail("incomplete or malformed PR listing".into()));
        }
        Ok(prs)
    }
}
