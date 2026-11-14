//! Whether GitHub still has a runner's registration: the runner's own
//! `.runner` (what config.sh wrote: its id at GitHub, its name and where it's
//! registered) against the runners GitHub lists there.
//!
//! GitHub deletes a registration (a runner removed in its settings, or one
//! gone too long), and the runner then exits cleanly as it starts ("The
//! runner registration has been deleted from the server"). A unit that
//! starts it again each time it exits (a path unit on `.runner`) loops, and
//! however often it's restarted it takes no job: idle with jobs queued is
//! the wrong account of it, and a restart the wrong move.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::Value;

use crate::snapshot::every;
use crate::world::World;

/// What a pass found of a runner's registration.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Registered {
    /// Not looked at: the runner has no `.runner` in its config.
    #[default]
    Unchecked,
    /// GitHub lists it.
    Listed,
    /// GitHub's whole list of runners where it was registered lacks it.
    Gone(Registration),
    /// Not known, and why: its `.runner` or GitHub's list couldn't be read.
    /// Never taken for gone.
    Unknown(String),
}

/// A runner's registration, as its `.runner` has it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registration {
    /// Its id at GitHub, and its name there.
    pub id: u64,
    pub name: String,
    /// Where it's registered: OWNER/REPO, or an organization's OWNER.
    pub scope: String,
}

impl Registration {
    /// The settings page that lists the scope's runners, where one is
    /// registered again.
    pub fn page(&self) -> String {
        if self.scope.contains('/') {
            format!("https://github.com/{}/settings/actions/runners", self.scope)
        } else {
            format!(
                "https://github.com/organizations/{}/settings/actions/runners",
                self.scope
            )
        }
    }
}

/// The registration a runner's `.runner` at `path` holds.
pub fn read(path: &Path) -> Result<Registration, String> {
    let at = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let text = std::fs::read(path).map_err(|e| at(&e))?;
    // config.sh writes it with a byte-order mark.
    let text = text.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&text);
    let v: Value = serde_json::from_slice(text).map_err(|e| at(&e))?;
    let id = v
        .get("agentId")
        .and_then(Value::as_u64)
        .ok_or_else(|| at(&"no agentId"))?;
    let name = v
        .get("agentName")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let url = v
        .get("gitHubUrl")
        .and_then(Value::as_str)
        .ok_or_else(|| at(&"no gitHubUrl"))?;
    Ok(Registration {
        id,
        name,
        scope: scope(url).ok_or_else(|| {
            at(&format!(
                "it's registered with {url}, neither a repo nor an organization on github.com"
            ))
        })?,
    })
}

/// OWNER/REPO or OWNER, from where a runner was registered; none for an
/// enterprise, or a host other than github.com, whose lists this doesn't
/// read.
fn scope(url: &str) -> Option<String> {
    let path = url
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/');
    let parts: Vec<&str> = path.split('/').collect();
    let plain = |s: &&str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    match parts.as_slice() {
        ["enterprises", ..] => None,
        [_] | [_, _] if parts.iter().all(plain) => Some(path.to_owned()),
        _ => None,
    }
}

/// The ids of the runners GitHub lists for each scope a pass has asked
/// about, each list read once and whole.
#[derive(Default)]
pub struct Lists(BTreeMap<String, Result<BTreeSet<u64>, String>>);

impl Lists {
    /// Whether GitHub still lists the runner whose `.runner` is at `path`.
    pub fn check(&mut self, world: &dyn World, path: &Path) -> Registered {
        let reg = match read(path) {
            Ok(r) => r,
            Err(e) => return Registered::Unknown(e),
        };
        let listed = self
            .0
            .entry(reg.scope.clone())
            .or_insert_with(|| ids(world, &reg.scope));
        match listed {
            Ok(ids) if ids.contains(&reg.id) => Registered::Listed,
            Ok(_) => Registered::Gone(reg),
            Err(e) => Registered::Unknown(format!("GitHub's runners for {}: {e}", reg.scope)),
        }
    }
}

/// Every runner GitHub lists for the scope, by id: all of them, or an error.
fn ids(world: &dyn World, scope: &str) -> Result<BTreeSet<u64>, String> {
    let path = if scope.contains('/') {
        format!("repos/{scope}/actions/runners")
    } else {
        format!("orgs/{scope}/actions/runners")
    };
    every(world, &path, "runners")?
        .iter()
        .map(|r| {
            r.get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| "a runner without an id".to_owned())
        })
        .collect()
}

#[cfg(test)]
pub mod tests {
    use serde_json::json;

    use super::{Lists, Registered, Registration, read, scope};
    use crate::snapshot::tests::Fake;

    /// A `.runner` as config.sh writes it, byte-order mark and all.
    pub fn dot_runner(dir: &std::path::Path, name: &str, id: u64, url: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        let body = json!({"agentId": id, "agentName": name, "poolId": 1, "gitHubUrl": url,
                          "workFolder": "_work"});
        std::fs::write(&path, format!("\u{feff}{body}")).unwrap();
        path
    }

    /// GitHub's list of the repo's runners, by id.
    pub fn listing(scope: &str, ids: &[u64]) -> (String, Result<serde_json::Value, String>) {
        let kind = if scope.contains('/') { "repos" } else { "orgs" };
        (
            format!("{kind}/{scope}/actions/runners?per_page=100&page=1"),
            Ok(json!({"total_count": ids.len(),
                      "runners": ids.iter().map(|id| json!({"id": id, "name": "x"})).collect::<Vec<_>>()})),
        )
    }

    // Where a runner was registered: a repo, or an organization; nothing
    // else is read.
    #[test]
    fn scopes() {
        assert_eq!(scope("https://github.com/sam/r").as_deref(), Some("sam/r"));
        assert_eq!(scope("https://github.com/sam/").as_deref(), Some("sam"));
        for url in [
            "https://github.com/enterprises/big",
            "https://git.example.com/sam/r",
            "https://github.com/sam/r/extra",
            "https://github.com/sam/r?x=1",
            "https://github.com/",
        ] {
            assert_eq!(scope(url), None, "{url}");
        }
    }

    // A runner GitHub lists is registered; one missing from the whole list
    // where it was registered is gone; one whose `.runner` or list can't be
    // read is unknown, never gone. Each scope's list is read once a pass.
    #[test]
    fn check() {
        let dir = tempfile::tempdir().unwrap();
        let one = dot_runner(dir.path(), "one", 21, "https://github.com/sam/r");
        let two = dot_runner(dir.path(), "two", 22, "https://github.com/sam/r");
        let org = dot_runner(dir.path(), "org", 5, "https://github.com/sam");
        let mut w = Fake::default();
        w.github
            .extend([listing("sam/r", &[22, 23]), listing("sam", &[5])]);
        let mut lists = Lists::default();
        assert_eq!(
            lists.check(&w, &one),
            Registered::Gone(Registration {
                id: 21,
                name: "one".into(),
                scope: "sam/r".into()
            })
        );
        assert_eq!(lists.check(&w, &two), Registered::Listed);
        assert_eq!(lists.check(&w, &org), Registered::Listed);
        // Read once, whatever it said.
        w.github.clear();
        assert_eq!(lists.check(&w, &two), Registered::Listed);

        // A list GitHub won't give, a page of it short, or a `.runner` not
        // there: unknown.
        let mut lists = Lists::default();
        let Registered::Unknown(why) = lists.check(&w, &one) else {
            panic!("an unread list taken for an answer");
        };
        assert!(why.contains("HTTP 404"), "{why}");
        w.github.insert(
            "repos/sam/r/actions/runners?per_page=100&page=1".into(),
            Ok(json!({"total_count": 101,
                      "runners": (0..100).map(|i| json!({"id": 100 + i})).collect::<Vec<_>>()})),
        );
        assert!(matches!(
            Lists::default().check(&w, &two),
            Registered::Unknown(_)
        ));
        assert!(matches!(
            Lists::default().check(&w, &dir.path().join("none")),
            Registered::Unknown(_)
        ));
        let odd = dot_runner(dir.path(), "odd", 1, "https://github.com/enterprises/big");
        let Registered::Unknown(why) = Lists::default().check(&w, &odd) else {
            panic!("an enterprise's runner checked");
        };
        assert!(why.contains("neither a repo nor an organization"), "{why}");
    }

    #[test]
    fn reads_without_the_mark_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".runner");
        std::fs::write(
            &path,
            r#"{"agentId": 3, "gitHubUrl": "https://github.com/sam/r"}"#,
        )
        .unwrap();
        let r = read(&path).unwrap();
        assert_eq!((r.id, r.scope.as_str()), (3, "sam/r"));
        assert_eq!(
            r.page(),
            "https://github.com/sam/r/settings/actions/runners"
        );
    }
}
