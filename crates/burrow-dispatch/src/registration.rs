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
/// Only a whole list says a runner is gone, so it's read strictly: each
/// page must have its `runners` array and `total_count`, each entry its id,
/// and the pages must hold exactly the total, read up to it. Anything short
/// of that (a malformed page, one short before the total's reached, a total
/// that moves) is an error, which leaves the registration unknown.
fn ids(world: &dyn World, scope: &str) -> Result<BTreeSet<u64>, String> {
    const PER: usize = 100;
    let path = if scope.contains('/') {
        format!("repos/{scope}/actions/runners")
    } else {
        format!("orgs/{scope}/actions/runners")
    };
    let mut out = BTreeSet::new();
    let mut read = 0u64;
    let mut total = None;
    for page in 1.. {
        let got = world.github(&format!("{path}?per_page={PER}&page={page}"))?;
        let entries = got
            .get("runners")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("page {page} has no list of runners"))?;
        let count = got
            .get("total_count")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("page {page} has no total_count"))?;
        if *total.get_or_insert(count) != count {
            return Err(format!(
                "its total moved from {} to {count} while it was read",
                total.unwrap_or(count)
            ));
        }
        for r in entries {
            let id = r
                .get("id")
                .and_then(Value::as_u64)
                .ok_or_else(|| format!("page {page} has a runner without an id"))?;
            out.insert(id);
        }
        read += entries.len() as u64;
        if read >= count {
            break;
        }
        if entries.len() < PER {
            return Err(format!(
                "page {page} ends the list at {read} of the {count} runners it counts"
            ));
        }
    }
    let count = total.unwrap_or(0);
    if read != count || out.len() as u64 != count {
        return Err(format!(
            "it lists {read} runners ({} ids) where it counts {count}",
            out.len()
        ));
    }
    Ok(out)
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

    // Only a whole list says a runner is gone, a repo's or an
    // organization's alike: one read across its pages to exactly the total
    // it counts. A page without its runners or its total, a runner without
    // an id, a page short before the total, a total that moves between
    // pages, more runners or fewer distinct ids than it counts: unknown.
    #[test]
    fn whole_lists() {
        let dir = tempfile::tempdir().unwrap();
        let page = |kind: &str, scope: &str, n: u32| {
            format!("{kind}/{scope}/actions/runners?per_page=100&page={n}")
        };
        let runners = |from: u64, n: u64| -> Vec<serde_json::Value> {
            (from..from + n).map(|id| json!({"id": id})).collect()
        };
        for (kind, scope, url) in [
            ("repos", "sam/r", "https://github.com/sam/r"),
            ("orgs", "sam", "https://github.com/sam"),
        ] {
            let dot = dot_runner(dir.path(), "box", 21, url);
            let check = |pages: &[serde_json::Value]| {
                let mut w = Fake::default();
                for (i, p) in pages.iter().enumerate() {
                    w.github
                        .insert(page(kind, scope, i as u32 + 1), Ok(p.clone()));
                }
                Lists::default().check(&w, &dot)
            };

            // Whole: over two pages, or none at all.
            let two = [
                json!({"total_count": 150, "runners": runners(100, 100)}),
                json!({"total_count": 150, "runners": runners(200, 50)}),
            ];
            assert!(matches!(check(&two), Registered::Gone(_)), "{scope}");
            let mut found = two.clone();
            found[1]["runners"][0]["id"] = json!(21);
            assert_eq!(check(&found), Registered::Listed, "{scope}");
            assert!(
                matches!(
                    check(&[json!({"total_count": 0, "runners": []})]),
                    Registered::Gone(_)
                ),
                "{scope}"
            );

            for (what, pages, why) in [
                (
                    "no runners",
                    vec![json!({"total_count": 0})],
                    "no list of runners",
                ),
                (
                    "runners not a list",
                    vec![json!({"total_count": 1, "runners": {"id": 22}})],
                    "no list of runners",
                ),
                (
                    "no total",
                    vec![json!({"runners": [{"id": 22}]})],
                    "no total_count",
                ),
                (
                    "a runner without an id",
                    vec![json!({"total_count": 2, "runners": [{"id": 22}, {"name": "x"}]})],
                    "without an id",
                ),
                (
                    "short of its total",
                    vec![json!({"total_count": 101, "runners": []})],
                    "ends the list at 0 of the 101",
                ),
                (
                    "a page short of its total",
                    vec![
                        json!({"total_count": 250, "runners": runners(100, 100)}),
                        json!({"total_count": 250, "runners": runners(200, 50)}),
                    ],
                    "page 2 ends the list at 150 of the 250",
                ),
                (
                    "a total that moves",
                    vec![
                        json!({"total_count": 150, "runners": runners(100, 100)}),
                        json!({"total_count": 140, "runners": runners(200, 40)}),
                    ],
                    "moved from 150 to 140",
                ),
                (
                    "more than its total",
                    vec![json!({"total_count": 1, "runners": runners(22, 2)})],
                    "lists 2 runners",
                ),
                (
                    "an id twice",
                    vec![json!({"total_count": 2, "runners": [{"id": 22}, {"id": 22}]})],
                    "(1 ids)",
                ),
            ] {
                let Registered::Unknown(e) = check(&pages) else {
                    panic!("{scope}, {what}: taken for an answer");
                };
                assert!(e.contains(why), "{scope}, {what}: {e}");
            }
        }
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
