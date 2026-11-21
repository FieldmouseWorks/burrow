//! Stopping a runner GitHub has deleted the registration of, and the path
//! unit that starts it again (rules.rs's `Move::Halt`). What the pass saw
//! may have moved on by the time it acts, and while it acts: a switch may
//! have begun, or the runner been registered again, its `.runner` rewritten
//! or removed. So GitHub's list is read again just before, and the local
//! guards are looked at after that read and before each stop; one that no
//! longer holds stops what's left.

use std::path::Path;

use crate::config::Config;
use crate::registration::{self, Lists, Registered, Registration};
use crate::world::World;

/// Whether the halt is still called for: GitHub's whole list, read again,
/// still lacks `reg`, and no switch has begun nor `.runner` changed, before
/// that read or during it.
pub fn still(
    cfg: &Config,
    world: &dyn World,
    unit: &str,
    reg: &Registration,
) -> Result<(), String> {
    let path = dot_runner(cfg, unit)?;
    held(world, path, reg)?;
    match Lists::default().check(world, path) {
        Registered::Gone(now) if now == *reg => {}
        Registered::Gone(_) => return Err(changed()),
        Registered::Listed => return Err("it's registered again".into()),
        Registered::Unknown(e) => return Err(e),
        Registered::Unchecked => return Err("its registration isn't known".into()),
    }
    // Reading the list takes a while: whatever began meanwhile counts.
    held(world, path, reg)
}

/// Stops each of `stop` in turn, the guards looked at before each: once one
/// no longer holds, nothing more is stopped. A stop that fails doesn't keep
/// the rest from being tried, and each failure is said.
pub fn stop(
    cfg: &Config,
    world: &dyn World,
    unit: &str,
    reg: &Registration,
    stop: &[String],
) -> Result<(), String> {
    let path = dot_runner(cfg, unit)?;
    let mut failed = Vec::new();
    for (i, u) in stop.iter().enumerate() {
        if let Err(why) = held(world, path, reg) {
            failed.push(format!("{} not stopped: {why}", stop[i..].join(" and ")));
            break;
        }
        if let Err(e) = world.restart(u, "stop") {
            failed.push(format!("{u}: {e}"));
        }
    }
    if failed.is_empty() {
        Ok(())
    } else {
        Err(failed.join("; "))
    }
}

/// The local guards: no switch in flight (it starts what it changes
/// itself), and the runner's `.runner` still the registration found gone.
fn held(world: &dyn World, path: &Path, reg: &Registration) -> Result<(), String> {
    if world.switching() {
        return Err("a switch is in flight".into());
    }
    match registration::read(path) {
        Ok(now) if now == *reg => Ok(()),
        Ok(_) => Err(changed()),
        Err(e) => Err(format!("its .runner can't be read: {e}")),
    }
}

fn changed() -> String {
    "its .runner holds another registration now".into()
}

fn dot_runner<'a>(cfg: &'a Config, unit: &str) -> Result<&'a Path, String> {
    cfg.runners
        .get(unit)
        .and_then(|r| r.registration.as_deref())
        .ok_or_else(|| "its registration isn't known".to_owned())
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    use serde_json::{Value, json};

    use burrow_watchdog::state::load;
    use burrow_watchdog::watch;

    use crate::config::Config;
    use crate::pass::tests::{QUEUED, idle, repo};
    use crate::pass::{run, status};
    use crate::registration::tests::{dot_runner, listing};
    use crate::snapshot::tests::{Fake, config, job, page, runs};
    use crate::state::State;
    use crate::world::{Unit, World};

    const T: f64 = 1_793_293_200.0;

    /// What changes under a halt.
    #[derive(Clone, Debug)]
    enum Change {
        /// A switch begins.
        Switch,
        /// The runner is registered again: `.runner` rewritten with this id,
        /// where it's registered.
        Rewrite(u64, &'static str),
        /// `.runner` removed.
        Remove,
        /// GitHub's list answers this from now on.
        Answer(Value),
    }

    /// A machine where `change` happens on the `n`th read of the runner
    /// list (1, the pass's own look; 2, its look again before the halt), or
    /// as `unit` is stopped; and whose stops of the units in `fails` fail.
    struct Moving {
        w: Fake,
        list: String,
        dot: PathBuf,
        on_read: Option<(usize, Change)>,
        on_stop: Option<(&'static str, Change)>,
        fails: BTreeMap<&'static str, &'static str>,
        reads: Cell<usize>,
        switching: Cell<bool>,
        answer: RefCell<Option<Value>>,
    }

    impl Moving {
        fn new(w: Fake, list: String, dot: PathBuf) -> Self {
            Moving {
                w,
                list,
                dot,
                on_read: None,
                on_stop: None,
                fails: BTreeMap::new(),
                reads: Cell::new(0),
                switching: Cell::new(false),
                answer: RefCell::new(None),
            }
        }

        fn apply(&self, c: &Change) {
            match c {
                Change::Switch => self.switching.set(true),
                Change::Rewrite(id, url) => {
                    let dir = self.dot.parent().unwrap();
                    let name = self.dot.file_name().unwrap().to_str().unwrap();
                    dot_runner(dir, name, *id, url);
                }
                Change::Remove => std::fs::remove_file(&self.dot).unwrap(),
                Change::Answer(v) => *self.answer.borrow_mut() = Some(v.clone()),
            }
        }

        fn stops(&self) -> Vec<String> {
            self.w
                .did
                .borrow()
                .iter()
                .filter(|d| d.starts_with("stop"))
                .cloned()
                .collect()
        }
    }

    impl World for Moving {
        fn github(&self, path: &str) -> Result<Value, String> {
            if path == self.list {
                let n = self.reads.get() + 1;
                self.reads.set(n);
                if let Some((at, c)) = &self.on_read
                    && *at == n
                {
                    self.apply(c);
                }
                if let Some(v) = self.answer.borrow().clone() {
                    return Ok(v);
                }
            }
            self.w.github(path)
        }
        fn unit(&self, unit: &str) -> Unit {
            self.w.unit(unit)
        }
        fn running(&self) -> Result<String, String> {
            self.w.running()
        }
        fn switching(&self) -> bool {
            self.switching.get() || self.w.switching()
        }
        fn restart(&self, unit: &str, verb: &str) -> Result<(), String> {
            self.w.restart(unit, verb)?;
            if let Some((u, c)) = &self.on_stop
                && *u == unit
            {
                self.apply(c);
            }
            self.fails
                .get(unit)
                .map_or(Ok(()), |e| Err((*e).to_owned()))
        }
        fn cancel(&self, repo: &str, run: u64) -> Result<(), String> {
            self.w.cancel(repo, run)
        }
        fn kick(&self, path: &str) {
            self.w.kick(path)
        }
        fn instances(&self, template: &str) -> Result<Vec<String>, String> {
            self.w.instances(template)
        }
        fn fresh(&self, f: &crate::config::Fresh, instance: &str) -> Result<(), String> {
            self.w.fresh(f, instance)
        }
        fn sweep(&self, f: &crate::config::Fresh) -> Vec<String> {
            self.w.sweep(f)
        }
    }

    /// remi's runner of 2026-11-13: GitHub deleted its registration (21 in
    /// sam/r, which lists only 22), it exits as it starts and `ci.path`
    /// starts it again, a job queued for it. Started also by the units in
    /// `also`, each up.
    fn deleted(dir: &Path, also: &[&str]) -> (Config, Moving) {
        let mut cfg = config(dir);
        cfg.deploy = None;
        cfg.runners.retain(|u, _| u == "ci");
        let dot = dot_runner(dir, "box", 21, "https://github.com/sam/r");
        cfg.runners.get_mut("ci").unwrap().registration = Some(dot.clone());
        let mut w = repo();
        let (list, listed) = listing("sam/r", &[22]);
        w.github.insert(list.clone(), listed);
        w.github.insert(page(QUEUED, 1), runs(&[1]));
        w.github.insert(
            page("repos/sam/r/actions/runs/1/jobs?filter=latest", 1),
            Ok(json!({"jobs": [job("gate", "queued", "2026-10-29T16:00:00Z", &["sam"])]})),
        );
        let triggers: Vec<String> = also
            .iter()
            .map(|t| (*t).to_owned())
            .chain(["ci.path".to_owned()])
            .collect();
        for t in &triggers {
            w.units.insert(t.clone(), idle(0.0));
        }
        w.units.insert(
            "ci".into(),
            Unit {
                triggered_by: triggers,
                ..idle(0.0)
            },
        );
        std::fs::write(
            dir.join("state.json"),
            json!({"idle": {"ci": 0.0}}).to_string(),
        )
        .unwrap();
        (cfg, Moving::new(w, list, dot))
    }

    /// The last move's logged result.
    fn result(dir: &Path) -> String {
        let st: State = load(dir, "state.json");
        st.log.last().unwrap().result.clone()
    }

    // It isn't restarted however long jobs wait: its path unit and it are
    // stopped, and logged; the watchdog's wait and status say its
    // registration is gone.
    #[test]
    fn deleted_registration() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, w) = deleted(dir.path(), &[]);

        let report = status(&cfg, &w, T);
        assert!(
            report.contains(
                "idle, 1 queued for it\n  its registration is gone at GitHub: sam/r lists no runner 21 (box); register it again (https://github.com/sam/r/settings/actions/runners)\n"
            ),
            "{report}"
        );
        assert!(report.contains("would stop ci.path and ci"), "{report}");

        let (cfg, w) = deleted(dir.path(), &[]);
        run(&cfg, &w, T).unwrap();
        assert_eq!(
            *w.w.did.borrow(),
            ["stop ci.path", "stop ci", "kick watchdog"]
        );
        let st: State = load(dir.path(), "state.json");
        assert!(st.restarts.is_empty());
        assert_eq!(
            (st.log[0].what.as_str(), st.log[0].result.as_str()),
            ("stop ci.path and ci", "ok")
        );
        let (_, items) = watch::read(&dir.path().join("watch.json")).unwrap();
        assert!(
            items["runner/ci"]
                .phase
                .starts_with("its registration is gone at GitHub"),
            "{:?}",
            items["runner/ci"]
        );
    }

    // What the pass saw moves on before the halt, while GitHub's list is
    // read again, or between its stops: a switch begins, the runner is
    // registered again (in its list, or with a `.runner` of another id or
    // another place), or its `.runner` goes. Nothing more is stopped once
    // it has; and a list read again that isn't whole stops nothing.
    #[test]
    fn moved_on() {
        let incomplete = json!({"total_count": 101, "runners": []});
        let cases: Vec<(&str, Change, &str)> = vec![
            ("switch", Change::Switch, "a switch is in flight"),
            (
                "another id",
                Change::Rewrite(30, "https://github.com/sam/r"),
                "another registration",
            ),
            (
                "another place",
                Change::Rewrite(21, "https://github.com/sam/q"),
                "another registration",
            ),
            ("removed", Change::Remove, "can't be read"),
        ];
        for (name, change, why) in &cases {
            // During the look again: nothing stopped.
            let dir = tempfile::tempdir().unwrap();
            let (cfg, mut w) = deleted(dir.path(), &[]);
            w.on_read = Some((2, change.clone()));
            run(&cfg, &w, T).unwrap();
            assert_eq!(w.reads.get(), 2, "{name}");
            assert!(w.stops().is_empty(), "{name}: {:?}", w.stops());
            let r = result(dir.path());
            assert!(
                r.starts_with("not made: ") && r.contains(why),
                "{name}: {r}"
            );

            // Between the stops: the path unit stopped, the runner not.
            let dir = tempfile::tempdir().unwrap();
            let (cfg, mut w) = deleted(dir.path(), &[]);
            w.on_stop = Some(("ci.path", change.clone()));
            run(&cfg, &w, T).unwrap();
            assert_eq!(w.stops(), ["stop ci.path"], "{name}");
            let r = result(dir.path());
            assert!(
                r.starts_with("ci not stopped: ") && r.contains(why),
                "{name}: {r}"
            );
        }

        for (name, answer, why) in [
            (
                "listed",
                json!({"total_count": 1, "runners": [{"id": 21}]}),
                "it's registered again",
            ),
            ("incomplete", incomplete, "ends the list at 0 of the 101"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (cfg, mut w) = deleted(dir.path(), &[]);
            w.on_read = Some((2, Change::Answer(answer)));
            run(&cfg, &w, T).unwrap();
            assert!(w.stops().is_empty(), "{name}: {:?}", w.stops());
            let r = result(dir.path());
            assert!(
                r.starts_with("not made: ") && r.contains(why),
                "{name}: {r}"
            );
        }
    }

    // Something else that starts it (a timer, sorting ahead of its path
    // unit) is named in its wait and never stopped, so polkit's refusal
    // can't keep the path unit and the runner from being stopped; and a
    // stop that fails doesn't keep the next from being tried, and is said.
    #[test]
    fn other_triggers_and_failures() {
        let dir = tempfile::tempdir().unwrap();
        let (cfg, w) = deleted(dir.path(), &["a-recovery.timer"]);
        run(&cfg, &w, T).unwrap();
        assert_eq!(w.stops(), ["stop ci.path", "stop ci"]);
        assert_eq!(result(dir.path()), "ok");
        let (_, items) = watch::read(&dir.path().join("watch.json")).unwrap();
        assert!(
            items["runner/ci"]
                .phase
                .ends_with("; a-recovery.timer starts it too, which dispatch doesn't stop"),
            "{:?}",
            items["runner/ci"]
        );

        let dir = tempfile::tempdir().unwrap();
        let (cfg, mut w) = deleted(dir.path(), &[]);
        w.fails.insert("ci.path", "Access denied");
        run(&cfg, &w, T).unwrap();
        assert_eq!(w.stops(), ["stop ci.path", "stop ci"]);
        assert_eq!(result(dir.path()), "ci.path: Access denied");
    }
}
