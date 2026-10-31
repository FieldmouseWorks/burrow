//! The watchdog's config, as modules/watchdog.nix writes it: WATCHDOG_CONFIG
//! names the file.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    /// Its state: state.json, escalated.json, wake.json and the pass's lock.
    pub state_dir: PathBuf,
    pub sources: BTreeMap<String, Source>,
    /// proj, which runs its Claude jobs and says how each is doing.
    pub proj: PathBuf,
    /// The project its jobs run in.
    pub project: String,
    /// A job's turn's timeout and idle limit, in minutes.
    pub job_timeout: u64,
    pub job_idle: u64,
    /// The machinery's App its jobs act on GitHub as (proj run --app); None
    /// for none.
    pub app: Option<String>,
    /// burrow-events' kick port: `http://127.0.0.1:PORT/kick`.
    pub kick: String,
    /// Seconds a job has before a stall goes to the stuck issue (or the
    /// owner, when it's theirs: escalate in ladder.rs).
    pub escalate_after: f64,
    /// The steward's handler, kicked to publish the stuck issue; None when
    /// there's no steward to publish one.
    pub steward_kick: Option<String>,
    /// The steward's labels, which a job can put on a PR for the owner.
    pub labels: Option<Labels>,
    /// notify, which tells the owner; "" for none.
    pub notify: String,
    pub owner: String,
    pub host: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Its watch file.
    pub file: PathBuf,
    /// The source, in words, for when it's stuck itself.
    pub about: String,
    /// Where a job finds out why one of its items is stuck, in a sentence.
    pub look: String,
    /// The handler to kick when the source itself is stuck.
    pub kick: Option<String>,
    /// Seconds after its last write it's stuck itself; None: an old file is
    /// no news.
    pub stale: Option<f64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Labels {
    /// The label that lists a PR for its owner.
    pub owner: String,
    /// The label that holds a PR for its owner's OK.
    pub hold: String,
}

impl Config {
    /// The config WATCHDOG_CONFIG names.
    pub fn load() -> Result<Config, String> {
        let path = std::env::var_os("WATCHDOG_CONFIG").ok_or("WATCHDOG_CONFIG isn't set")?;
        let shown = path.to_string_lossy().into_owned();
        let text = std::fs::read(&path).map_err(|e| format!("{shown}: {e}"))?;
        serde_json::from_slice(&text).map_err(|e| format!("{shown}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    // The config as the module writes it; a field it doesn't know is a
    // module and a binary out of step, and fails rather than going unread.
    #[test]
    fn config() {
        let good = serde_json::json!({
            "stateDir": "/var/lib/burrow/watchdog",
            "sources": {"proj": {"file": "/w.json", "about": "proj's line", "look": "proj status.", "kick": null, "stale": null}},
            "proj": "/bin/proj", "project": "homelab", "jobTimeout": 60, "jobIdle": 20, "app": null,
            "kick": "http://127.0.0.1:8788/kick", "escalateAfter": 7200,
            "stewardKick": null, "labels": null, "notify": "", "owner": "Sam", "host": "box",
        });
        let cfg: Config = serde_json::from_value(good.clone()).unwrap();
        assert_eq!(cfg.sources["proj"].stale, None);
        let mut odd = good;
        odd["jobs"] = 2.into();
        assert!(serde_json::from_value::<Config>(odd).is_err());
    }
}
