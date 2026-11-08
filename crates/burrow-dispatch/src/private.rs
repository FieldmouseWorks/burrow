//! Repo-scoped JIT runners for private repositories discovered from each
//! covered owner's App installation. Numeric repository ids partition the
//! shared unit template; users and workspaces remain unique to each job.
use std::collections::BTreeMap;

use burrow_watchdog::watch::Item;
use serde_json::Value;

use crate::config::{Config, Fresh};
use crate::rules::{Decision, Move, book};
use crate::snapshot::{Pool, Snapshot};
use crate::state::State;
use crate::world::{Unit, World};

struct Scoped<'a> {
    world: &'a dyn World,
    owner: &'a str,
}
impl World for Scoped<'_> {
    fn github(&self, path: &str) -> Result<Value, String> {
        self.world.private_github(self.owner, path)
    }
    fn unit(&self, unit: &str) -> Unit {
        self.world.unit(unit)
    }
    fn instances(&self, template: &str) -> Result<Vec<String>, String> {
        self.world.instances(template)
    }
    fn running(&self) -> Result<String, String> {
        self.world.running()
    }
    fn switching(&self) -> bool {
        self.world.switching()
    }
    fn restart(&self, unit: &str, verb: &str) -> Result<(), String> {
        self.world.restart(unit, verb)
    }
    fn cancel(&self, repo: &str, run: u64) -> Result<(), String> {
        self.world.cancel(repo, run)
    }
    fn kick(&self, path: &str) {
        self.world.kick(path)
    }
    fn fresh(&self, f: &Fresh, instance: &str) -> Result<(), String> {
        self.world.fresh(f, instance)
    }
    fn sweep(&self, f: &Fresh) -> Vec<String> {
        self.world.sweep(f)
    }
}

/// Installation repositories include private user-account repositories;
/// users/OWNER/repos only lists public repositories, even with an App token.
fn discover(world: &dyn World, owner: &str) -> Result<Vec<(String, u64)>, String> {
    let mut repos = BTreeMap::new();
    for page in 1.. {
        let got = world.github(&format!(
            "installation/repositories?per_page=100&page={page}"
        ))?;
        let entries = got["repositories"]
            .as_array()
            .ok_or("repository list is missing")?;
        for r in entries {
            if r["private"] != true || r["archived"] == true || r["disabled"] == true {
                continue;
            }
            let name = r["full_name"]
                .as_str()
                .ok_or("repository name is missing")?;
            let (account, repo) = name.split_once('/').ok_or("invalid repository name")?;
            if !account.eq_ignore_ascii_case(owner) || repo.is_empty() || repo.contains('/') {
                continue;
            }
            let id = r["id"]
                .as_u64()
                .filter(|id| *id > 0)
                .ok_or("repository id is missing")?;
            repos.insert(name.to_owned(), id);
        }
        if entries.len() < 100 {
            break;
        }
    }
    Ok(repos.into_iter().collect())
}

fn discovery_key(owner: &str) -> String {
    format!("private/{}", owner.to_ascii_lowercase())
}

pub fn take(cfg: &Config, world: &dyn World, snap: &mut Snapshot) {
    let Some(base) = &cfg.fresh else {
        return;
    };
    let covered = cfg
        .private
        .as_ref()
        .map_or(&[][..], |p| p.owners.as_slice());
    let mut owners = std::collections::BTreeSet::new();
    for owner in covered {
        if !owners.insert(owner.to_ascii_lowercase()) {
            continue;
        }
        let scoped = Scoped { world, owner };
        let repos = match discover(&scoped, owner) {
            Ok(repos) => repos,
            Err(e) => {
                snap.unread.insert(discovery_key(owner), e);
                continue;
            }
        };
        for (repo, id) in repos {
            // An explicitly configured org pool already serves these jobs.
            if owner.eq_ignore_ascii_case(&base.owner)
                && base.repos.iter().any(|r| r.eq_ignore_ascii_case(&repo))
            {
                continue;
            }
            let mut f = base.clone();
            f.owner = repo.clone();
            f.repository = Some(repo.clone());
            f.scope = format!("{id}-");
            f.repos = vec![repo.clone()];
            f.idle = 0; // A repo runner cannot be prewarmed for another repo.
            let pool = match crate::snapshot::look(&scoped, &repo) {
                Ok((q, _)) => {
                    crate::snapshot::pool(&scoped, &f, &BTreeMap::from([(repo.as_str(), q)]))
                }
                Err(e) => {
                    snap.unread.insert(repo.clone(), e.clone());
                    Err(e)
                }
            };
            if let Err(e) = &pool {
                snap.unread.insert(repo.clone(), e.clone());
            }
            snap.private.push((f, pool));
        }
    }
    // A deleted, transferred or archived repo no longer appears in discovery.
    // Its idle runner still holds a slot until stopped; never stop its job.
    if !covered
        .iter()
        .any(|o| snap.unread.contains_key(&discovery_key(o)))
        && let Ok(instances) = world.instances(&base.template)
    {
        for instance in instances {
            if !base.owns(&instance) && !snap.private.iter().any(|(f, _)| f.owns(&instance)) {
                let name = format!("{}{instance}.service", base.template);
                snap.private_orphans.insert(name.clone(), world.unit(&name));
            }
        }
    }
}

fn up(pool: &Pool) -> usize {
    pool.units
        .values()
        .filter(|u| {
            u.pending
                || matches!(
                    u.state.as_str(),
                    "active" | "activating" | "deactivating" | "reloading"
                )
        })
        .count()
}

pub fn decide(
    cfg: &Config,
    snap: &Snapshot,
    st: &State,
    was: &BTreeMap<String, Item>,
    now: f64,
    d: &mut Decision,
) {
    let Some(base) = &cfg.fresh else {
        return;
    };
    // The org pool includes every loaded unit, so starts across all scopes
    // share one limit, including starts planned earlier in this pass.
    let Some(Ok(all)) = &snap.fresh else {
        d.state.private = st.private.clone();
        for (key, item) in was.iter().filter(|(key, _)| key.starts_with("private/")) {
            d.items.insert(key.clone(), item.clone());
        }
        return;
    };
    let mut room = base.most.saturating_sub(
        up(all)
            + d.moves
                .iter()
                .filter(|m| matches!(m, Move::Fresh { .. }))
                .count(),
    );
    for (unit, seen) in &snap.private_orphans {
        if seen.state == "active" && !seen.busy && seen.since.is_some_and(|t| now >= t + cfg.quiet)
        {
            d.moves.push(Move::Retire {
                unit: unit.clone(),
                why: "its repository no longer needs a runner".into(),
            });
        } else {
            book(d, now + cfg.quiet);
        }
    }
    let mut pools: Vec<_> = snap.private.iter().collect();
    pools.sort_by(|(_, a), (_, b)| {
        let oldest = |p: &Result<Pool, String>| {
            p.as_ref()
                .ok()
                .and_then(|p| p.queued.first())
                .map_or(f64::INFINITY, |j| j.at)
        };
        oldest(a).total_cmp(&oldest(b))
    });
    for (f, pool) in pools {
        let repo = f.repository.as_ref().expect("discovered repository");
        let key = format!("private/{repo}");
        let mut local = Decision::default();
        let state = State {
            fresh: st.private.get(repo).copied(),
            ..State::default()
        };
        let previous = was
            .get(&key)
            .map(|i| BTreeMap::from([("fresh".into(), i.clone())]))
            .unwrap_or_default();
        let mut limited = f.clone();
        limited.most = pool.as_ref().map_or(0, up) + room;
        crate::fresh::decide(
            cfg,
            &limited,
            pool.as_ref().ok(),
            pool.is_err(),
            (&state, &previous),
            now,
            &mut local,
        );
        for m in local.moves {
            d.moves.push(match m {
                Move::Fresh { instance, why } => {
                    room = room.saturating_sub(1);
                    Move::PrivateFresh {
                        repo: repo.clone(),
                        instance,
                        why,
                    }
                }
                other => other,
            });
        }
        if let Some(t) = local.wake {
            book(d, t);
        }
        if let Some(t) = local.state.fresh {
            d.state.private.insert(repo.clone(), t);
        }
        if let Some(item) = local.items.remove("fresh") {
            d.items.insert(key, item);
        }
        // Capacity becoming free and repo registration failing both need
        // another look; queued work must never silently lose its wake-up.
        if pool.as_ref().is_ok_and(|p| !p.queued.is_empty()) {
            book(d, now + cfg.quiet);
        }
    }
    // Discovery failures preserve each owner's existing waits.
    for (repo, since) in &st.private {
        let owner = repo.split('/').next().unwrap_or("");
        if snap.unread.contains_key(&discovery_key(owner)) {
            d.state.private.insert(repo.clone(), *since);
            let key = format!("private/{repo}");
            if let Some(item) = was.get(&key) {
                d.items.insert(key, item.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules;
    use crate::snapshot::tests::{Fake, config, fresh, job, runs};
    use serde_json::json;

    fn fixture(dir: &std::path::Path) -> (Config, Fake) {
        let mut cfg = config(dir);
        cfg.runners.clear();
        cfg.deploy = None;
        cfg.fresh = Some(fresh(dir));
        cfg.fresh.as_mut().unwrap().repos.clear();
        cfg.private = Some(crate::config::Private {
            owners: vec!["sam".into()],
            app: 12,
        });
        let mut w = Fake::default();
        let mut page = vec![json!({"private": false}); 99];
        page.push(json!({"private": true, "full_name": "sam/new", "id": 42}));
        w.github.insert(
            "installation/repositories?per_page=100&page=1".into(),
            Ok(json!({"repositories": page})),
        );
        w.github.insert(
            "installation/repositories?per_page=100&page=2".into(),
            Ok(json!({"repositories": [
                {"private":true,"full_name":"sam/second","id":43},
                {"private":true,"full_name":"other/no","id":44},
                {"private":true,"full_name":"sam/archive","id":45,"archived":true}
            ]})),
        );
        for repo in ["new", "second"] {
            w.github.insert(
                format!("repos/sam/{repo}"),
                Ok(json!({"default_branch": "main"})),
            );
            for status in ["queued", "in_progress", "pending"] {
                w.github.insert(
                    format!("repos/sam/{repo}/actions/runs?status={status}&per_page=100&page=1"),
                    runs(if status == "queued" { &[1] } else { &[] }),
                );
            }
            w.github.insert(
                format!("repos/sam/{repo}/actions/runs/1/jobs?filter=latest&per_page=100&page=1"),
                Ok(json!({"jobs": [
                    job("gate", "queued", "2026-10-29T17:00:00Z", &["fresh-box"]),
                    job("hosted", "queued", "2026-10-29T17:00:00Z", &["ubuntu-latest"]),
                    job("extra", "queued", "2026-10-29T17:00:00Z", &["fresh-box", "other"])
                ]})),
            );
        }
        (cfg, w)
    }

    #[test]
    fn new_private_repositories_get_scoped_runners_without_configuration() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, w) = fixture(dir.path());
        let snap = crate::snapshot::take(&cfg, &w);
        assert!(snap.unread.is_empty(), "{:?}", snap.unread);
        assert_eq!(snap.private.len(), 2);
        assert!(
            snap.private
                .iter()
                .all(|(_, p)| p.as_ref().unwrap().queued.len() == 1)
        );
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        let starts: Vec<_> = d
            .moves
            .iter()
            .filter_map(|m| match m {
                Move::PrivateFresh { repo, instance, .. } => {
                    Some((repo.as_str(), instance.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            starts,
            [
                ("sam/new", "42-1800000000-0"),
                ("sam/second", "43-1800000000-0")
            ]
        );
    }

    #[test]
    fn scopes_share_capacity_and_never_count_another_repos_idle_runner() {
        let dir = tempfile::tempdir().unwrap();
        let (mut cfg, mut w) = fixture(dir.path());
        cfg.fresh.as_mut().unwrap().most = 1;
        w.instances = vec!["42-1799999999-0".into()];
        w.units.insert(
            "fresh-runner@42-1799999999-0.service".into(),
            Unit {
                state: "active".into(),
                since: Some(1800000000.0),
                ..Unit::default()
            },
        );
        let snap = crate::snapshot::take(&cfg, &w);
        assert_eq!(snap.private[0].1.as_ref().unwrap().units.len(), 1);
        assert!(snap.private[1].1.as_ref().unwrap().units.is_empty());
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        assert!(
            !d.moves
                .iter()
                .any(|m| matches!(m, Move::PrivateFresh { .. } | Move::Fresh { .. }))
        );
        assert!(d.wake.is_some());
        // When the first repo's unit is gone, exactly one new repo runner fits.
        w.instances.clear();
        let snap = crate::snapshot::take(&cfg, &w);
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        assert_eq!(
            d.moves
                .iter()
                .filter(|m| matches!(m, Move::PrivateFresh { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn discovery_failure_is_a_visible_wait() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, mut w) = fixture(dir.path());
        w.github
            .remove("installation/repositories?per_page=100&page=2");
        let snap = crate::snapshot::take(&cfg, &w);
        assert!(snap.private.is_empty());
        assert!(snap.unread.contains_key("private/sam"));
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        assert!(d.items.contains_key("github/private/sam"));
    }
    #[test]
    fn private_demand_precedes_org_prewarming_and_retires_orphans() {
        let dir = tempfile::tempdir().unwrap();
        let (mut cfg, mut w) = fixture(dir.path());
        cfg.fresh.as_mut().unwrap().idle = 2;
        let snap = crate::snapshot::take(&cfg, &w);
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        assert_eq!(
            d.moves
                .iter()
                .filter(|m| matches!(m, Move::PrivateFresh { .. }))
                .count(),
            2
        );
        assert!(!d.moves.iter().any(|m| matches!(m, Move::Fresh { .. })));
        w.instances = vec!["99-1799999000-0".into()];
        w.units.insert(
            "fresh-runner@99-1799999000-0.service".into(),
            Unit {
                state: "active".into(),
                since: Some(1799999000.0),
                ..Unit::default()
            },
        );
        let snap = crate::snapshot::take(&cfg, &w);
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        assert!(d.moves.iter().any(|m| matches!(m, Move::Retire { unit, .. } if unit == "fresh-runner@99-1799999000-0.service")));
        w.units
            .get_mut("fresh-runner@99-1799999000-0.service")
            .unwrap()
            .busy = true;
        let snap = crate::snapshot::take(&cfg, &w);
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        assert!(!d.moves.iter().any(|m| matches!(m, Move::Retire { .. })));
    }
    #[test]
    fn starting_repo_runner_does_not_satisfy_an_org_queue() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, mut w) = fixture(dir.path());
        w.instances = vec!["42-1799999999-0".into()];
        w.units.insert(
            "fresh-runner@42-1799999999-0.service".into(),
            Unit {
                state: "activating".into(),
                since: Some(1800000000.0),
                ..Unit::default()
            },
        );
        let mut snap = crate::snapshot::take(&cfg, &w);
        for (_, pool) in &mut snap.private {
            pool.as_mut().unwrap().queued.clear();
        }
        snap.fresh
            .as_mut()
            .unwrap()
            .as_mut()
            .unwrap()
            .queued
            .push(crate::snapshot::Job {
                at: 1800000000.0,
                url: "https://github.com/sam/r/actions/jobs/1".into(),
            });
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        assert_eq!(
            d.moves
                .iter()
                .filter(|m| matches!(m, Move::Fresh { .. }))
                .count(),
            1
        );
    }
    #[test]
    fn turning_discovery_off_retires_its_idle_runners() {
        let dir = tempfile::tempdir().unwrap();
        let (mut cfg, mut w) = fixture(dir.path());
        cfg.private = None;
        w.instances = vec!["42-1799999000-0".into()];
        w.units.insert(
            "fresh-runner@42-1799999000-0.service".into(),
            Unit {
                state: "active".into(),
                since: Some(1799999000.0),
                ..Unit::default()
            },
        );
        let snap = crate::snapshot::take(&cfg, &w);
        let d = rules::decide(
            &cfg,
            &snap,
            &State::default(),
            &BTreeMap::new(),
            1800000000.0,
        );
        assert!(d.moves.iter().any(|m| matches!(m, Move::Retire { .. })));
    }
}
