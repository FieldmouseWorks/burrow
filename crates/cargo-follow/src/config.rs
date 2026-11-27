//! What modules/updates/follow.nix tells a pass, as JSON.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// The manifests whose `metadata.follow` it keeps.
    pub manifests: Vec<Manifest>,
    /// Its clones, cargo's home and the lock every pass holds.
    pub work: PathBuf,
    /// The handler's wake file (burrow-events), {repo: epoch} with "" for
    /// the full pass: a pass GitHub's rate limit held off is booked there
    /// for when it lifts.
    pub wake: PathBuf,
    /// Labels on an open PR that keep a pass from pushing to it: the
    /// train's, whose candidate a new head would throw away.
    pub frozen: Vec<String>,
    /// The steward's label that takes a PR first in the landing line
    /// (burrow.autoland.labels.first): put on follow PRs in `pipeline_repos`.
    #[serde(default)]
    pub first: Option<String>,
    /// Repos whose follow PRs carry `first` (OWNER/NAME, any case):
    /// `burrow.autoland.pipelineRepos` and `leadRepos`.
    #[serde(default)]
    pub pipeline_repos: Vec<String>,
}

/// A manifest: its repo, OWNER/NAME, and its path there.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub repo: String,
    pub path: String,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// The `first` label to put on follow PRs in `repo` (or its configured
    /// name), when `first` is non-empty and either name is in `pipeline_repos`.
    pub fn first_for(&self, repo: &str, configured: &str) -> Option<&str> {
        let label = self.first.as_deref().filter(|l| !l.is_empty())?;
        self.pipeline_repos
            .iter()
            .any(|r| r.eq_ignore_ascii_case(repo) || r.eq_ignore_ascii_case(configured))
            .then_some(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_what_it_knows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = serde_json::json!({
            "manifests": [{"repo": "sam/home", "path": "crates/Cargo.toml"}],
            "work": "/w",
            "wake": "/w/wake.json",
            "frozen": ["waiting: train"],
            "first": "first",
            "pipelineRepos": ["Sam/Home"],
        });
        std::fs::write(&path, config.to_string()).unwrap();
        let read = Config::read(&path).unwrap();
        assert_eq!(read.manifests[0].path, "crates/Cargo.toml");
        assert_eq!(read.first.as_deref(), Some("first"));
        assert_eq!(read.pipeline_repos, ["Sam/Home"]);
        config["dry_run"] = true.into();
        std::fs::write(&path, config.to_string()).unwrap();
        let e = Config::read(&path).unwrap_err();
        assert!(e.contains("unknown field `dry_run`"), "{e}");
    }

    #[test]
    fn first_for_matches_pipeline_repos_case_insensitively() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "manifests": [],
                "work": "/w",
                "wake": "/w/wake.json",
                "frozen": [],
            })
            .to_string(),
        )
        .unwrap();
        let mut cfg = Config::read(&path).unwrap();
        assert_eq!(cfg.first, None);
        assert!(cfg.pipeline_repos.is_empty());
        assert_eq!(cfg.first_for("sam/home", "sam/home"), None);

        cfg.first = Some("first".into());
        cfg.pipeline_repos = vec!["FieldmouseWorks/Burrow".into()];
        assert_eq!(
            cfg.first_for("fieldmouseworks/burrow", "fieldmouseworks/burrow"),
            Some("first")
        );
        assert_eq!(
            cfg.first_for("fieldmouseworks/renamed", "FieldmouseWorks/Burrow"),
            Some("first")
        );
        assert_eq!(cfg.first_for("sam/other", "sam/other"), None);

        cfg.first = Some(String::new());
        assert_eq!(
            cfg.first_for("fieldmouseworks/burrow", "fieldmouseworks/burrow"),
            None
        );
    }
}
