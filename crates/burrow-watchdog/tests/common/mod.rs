//! The machine a pass runs on, with proj, burrow-events' kick port and
//! notify stood in for. notify stands in as its contract has it
//! (modules/notify, whose own check holds it to that): `pending` says
//! whether a notice it was asked to open may still be open, and nothing gets
//! through while GitHub is down. proj's `usage` says what `usage` holds: ""
//! for a machine with Claude usage to spare.

#![allow(dead_code)]

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;

use burrow_watchdog::MIN;
use burrow_watchdog::config::{Config, Labels, Source};
use burrow_watchdog::ladder::run;
use burrow_watchdog::state::{Stall, State, load};
use burrow_watchdog::world::World;
use serde_json::{Value, json};

/// The tests' clock.
pub const T: f64 = 1_800_000_000.0;
pub const H: f64 = 3600.0;

#[derive(Default)]
pub struct Fake {
    /// Every proj call's arguments.
    pub proj: RefCell<Vec<Vec<String>>>,
    /// What `proj state JOB` answers ("done" for one it doesn't name, failed
    /// once it's withdrawn), and whether `proj run` or `proj withdraw` fails.
    pub states: RefCell<HashMap<String, String>>,
    pub refuse: Cell<bool>,
    /// What `proj usage` prints.
    pub usage: RefCell<String>,
    pub stuck: Cell<bool>,
    pub kicks: RefCell<Vec<String>>,
    /// notify's calls but `pending`.
    pub notices: RefCell<Vec<Vec<String>>>,
    /// GitHub: down or not, and the notices open on it.
    pub down: Cell<bool>,
    pub opened: RefCell<BTreeSet<String>>,
    /// The notices notify asked to open and hasn't seen closed.
    pub kept: RefCell<BTreeSet<String>>,
}

impl World for Fake {
    fn proj(&self, args: &[&str]) -> (bool, String) {
        let mut log = self.proj.borrow_mut();
        log.push(args.iter().map(|a| a.to_string()).collect());
        match args[0] {
            // proj takes the hold (unless it refuses everything), and says
            // it's out from then on.
            "usage" if args.get(1) == Some(&"adopt") => {
                if self.refuse.get() {
                    return (false, String::new());
                }
                *self.usage.borrow_mut() = format!(
                    r#"{{"until": {}, "said": "a usage limit an earlier proj or watchdog recorded"}}"#,
                    args[2]
                );
                (true, String::new())
            }
            "usage" => (true, self.usage.borrow().clone()),
            "run" if self.refuse.get() => (false, String::new()),
            "run" => {
                let n = log.iter().filter(|a| a[0] == "run").count();
                (true, format!("job-{n}"))
            }
            "withdraw" if self.stuck.get() => (false, String::new()),
            "withdraw" => {
                let mut states = self.states.borrow_mut();
                let said = match states.get(args[1]).map(String::as_str) {
                    Some("queued") => "withdrawn",
                    Some("running") => "running",
                    _ => "over",
                };
                if said == "withdrawn" {
                    states.insert(args[1].into(), "failed".into());
                }
                (true, said.into())
            }
            _ => (
                true,
                self.states
                    .borrow()
                    .get(args[1])
                    .cloned()
                    .unwrap_or("done".into()),
            ),
        }
    }

    fn kick(&self, path: &str) {
        self.kicks.borrow_mut().push(path.into());
    }

    fn notify(&self, args: &[&str]) -> Option<i32> {
        let key = args[1].to_owned();
        if args[0] == "pending" {
            return Some(if self.kept.borrow().contains(&key) {
                0
            } else {
                1
            });
        }
        self.notices
            .borrow_mut()
            .push(args.iter().map(|a| a.to_string()).collect());
        match args[0] {
            "open" if !self.down.get() && !self.opened.borrow().contains(&key) => {
                self.kept.borrow_mut().insert(key.clone());
                self.opened.borrow_mut().insert(key);
            }
            "close" if !self.down.get() && self.kept.borrow().contains(&key) => {
                self.opened.borrow_mut().remove(&key);
                self.kept.borrow_mut().remove(&key);
            }
            _ => {}
        }
        Some(0)
    }
}

/// A machine with a steward (whose watch file it should write at least
/// daily) and proj's line, everything else stood in for.
pub struct Machine {
    pub _dir: tempfile::TempDir,
    pub cfg: Config,
    pub world: Fake,
    pub steward: PathBuf,
    pub line: PathBuf,
}

impl Machine {
    pub fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        fs::create_dir(&state).unwrap();
        let (steward, line) = (
            dir.path().join("steward.json"),
            dir.path().join("line.json"),
        );
        let cfg = Config {
            settings: Some(burrow_settings::Config {
                overlay: dir.path().join("overlay.json"),
                allowed: vec!["sam/r".into(), "sam/q".into()],
                pipeline_repos: vec!["sam/r".into()],
                ..Default::default()
            }),
            state_dir: state,
            sources: BTreeMap::from([
                (
                    "autoland".into(),
                    Source {
                        file: steward.clone(),
                        about: "the PR steward".into(),
                        look: "journalctl -u 'autoland*'.".into(),
                        kick: Some("autoland".into()),
                        stale: Some(86400.0),
                    },
                ),
                (
                    "proj".into(),
                    Source {
                        file: line.clone(),
                        about: "proj's line".into(),
                        look: "proj status.".into(),
                        kick: None,
                        stale: None,
                    },
                ),
            ]),
            proj: "proj".into(),
            project: "homelab".into(),
            job_timeout: 60,
            job_idle: 20,
            app: None,
            kick: "http://127.0.0.1:8788/kick".into(),
            escalate_after: 7200.0,
            steward_kick: Some("autoland".into()),
            labels: Some(Labels {
                owner: "needs-owner".into(),
                hold: "hold".into(),
            }),
            notify: "notify".into(),
            owner: "Sam".into(),
            host: "box".into(),
            anomalies: None,
        };
        Machine {
            _dir: dir,
            cfg,
            world: Fake::default(),
            steward,
            line,
        }
    }

    pub fn write(&self, path: &PathBuf, items: Value, at: f64) {
        fs::write(path, json!({"at": at, "items": items}).to_string()).unwrap();
    }

    /// A pass at `now`. The steward writes its file on every pass of its
    /// own, so it's fresh then, unless the test is about one that isn't.
    pub fn pass(&self, now: f64, fresh: bool) -> (Option<f64>, BTreeMap<String, Stall>) {
        if fresh && let Ok(text) = fs::read(&self.steward) {
            let got: Value = serde_json::from_slice(&text).unwrap();
            self.write(&self.steward, got["items"].clone(), now);
        }
        run(&self.cfg, &self.world, now).unwrap();
        let wake: Value = load(&self.cfg.state_dir, "wake.json");
        (wake.get("").and_then(Value::as_f64), self.state().stalls)
    }

    pub fn run(&self, now: f64) -> (Option<f64>, BTreeMap<String, Stall>) {
        self.pass(now, true)
    }

    pub fn state(&self) -> State {
        load(&self.cfg.state_dir, "state.json")
    }

    /// What the stuck issue is to carry.
    pub fn escalated(&self) -> Vec<[String; 3]> {
        load(&self.cfg.state_dir, "escalated.json")
    }

    pub fn runs(&self) -> Vec<Vec<String>> {
        self.world
            .proj
            .borrow()
            .iter()
            .filter(|a| a[0] == "run")
            .cloned()
            .collect()
    }

    /// Every job it asked proj to withdraw, once an ask.
    pub fn withdrawals(&self) -> Vec<String> {
        self.world
            .proj
            .borrow()
            .iter()
            .filter(|a| a[0] == "withdraw")
            .map(|a| a[1].clone())
            .collect()
    }

    pub fn kicks(&self) -> Vec<String> {
        self.world.kicks.borrow().clone()
    }

    pub fn notices(&self) -> Vec<Vec<String>> {
        self.world.notices.borrow().clone()
    }

    pub fn opened(&self) -> usize {
        self.world.opened.borrow().len()
    }

    pub fn state_of(&self, job: &str, state: &str) {
        self.world
            .states
            .borrow_mut()
            .insert(job.into(), state.into());
    }

    /// A stall taken to the owner, in the fewest passes: when they heard.
    /// The stuck issue's own job waiting for its slot is one. The steward
    /// writes its file all the while, with nothing in it.
    pub fn told_at(&self) -> f64 {
        self.write(&self.steward, json!({}), T);
        self.write(&self.line, json!({"homelab-9": unstick(true)}), T);
        self.run(T);
        T
    }
}

pub fn item(since: f64, due: f64, kick: Option<&str>) -> Value {
    json!({"what": "PR #7 in sam/r (\"fix it\")", "link": "https://github.com/sam/r/pull/7",
           "phase": "Settled; waiting for CI: build", "since": since, "due": due, "kick": kick})
}

pub fn named(what: &str, phase: &str, since: f64, due: f64) -> Value {
    json!({"what": what, "link": null, "phase": phase, "since": since, "due": due, "kick": null})
}

/// The stuck issue's own job (proj run --unstick), past its deadline: waiting
/// for its slot, or at work.
pub fn unstick(line: bool) -> Value {
    let phase = if line {
        "turn 2 is waiting for the --unstick slot, which another --unstick turn holds"
    } else {
        "turn 2 is running"
    };
    let mut it = named("proj job homelab-9", phase, T - 3.0 * H, T - MIN);
    it["line"] = line.into();
    it["unstick"] = true.into();
    it
}
