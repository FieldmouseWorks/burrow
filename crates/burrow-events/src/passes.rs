//! Starting a handler's passes as they come due.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Map, Value};

use crate::handler::Handler;
use crate::line::{Line, Pass, Scope};
use crate::now;
use crate::routes::{Handlers, Lines};

/// What starts a pass: systemd, or a stand-in for it.
pub trait Units {
    /// The unit's ActiveState ("activating" while a oneshot runs).
    fn state(&self, unit: &str) -> String;
    /// Start it and wait for it to finish: whether it went through.
    fn start(&self, unit: &str) -> bool;
}

/// systemctl, from the service's PATH.
pub struct Systemctl;

impl Units for Systemctl {
    fn state(&self, unit: &str) -> String {
        Command::new("systemctl")
            .args(["show", "-P", "ActiveState", unit])
            .stderr(Stdio::inherit())
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_default()
    }

    fn start(&self, unit: &str) -> bool {
        Command::new("systemctl")
            .args(["start", unit])
            .status()
            .is_ok_and(|s| s.success())
    }
}

/// The wake file's wake-ups; none when it's missing or isn't an object.
pub fn booked(path: &Path) -> Map<String, Value> {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| match v {
            Value::Object(m) => Some(m),
            _ => None,
        })
        .unwrap_or_default()
}

/// Tell a pass about to start which issues to look at, if its handler is
/// told them: its file has their numbers, one a line, or isn't there for a
/// look at everything (whatever a pass before it left). The file to remove
/// once it's done; a pass that can't be told looks at everything.
fn tell(name: &str, h: &Handler, pass: &Pass, scope: &Scope) -> Option<PathBuf> {
    let f = h.issue_file(pass)?;
    let told = match scope {
        Scope::All => match fs::remove_file(&f) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => return None,
        },
        Scope::Issues(ns) => {
            // Whole or not at all: a pass reading half the list would leave
            // the rest of its issues unlooked at.
            let text: String = ns.iter().map(|n| format!("{n}\n")).collect();
            let new = f.with_extension("new");
            f.parent()
                .map_or(Ok(()), fs::create_dir_all)
                .and_then(|()| fs::write(&new, text))
                .and_then(|()| fs::rename(&new, &f))
        }
    };
    match told {
        Ok(()) => Some(f),
        Err(e) => {
            eprintln!("{name}: {}: {e}", f.display());
            None
        }
    }
}

/// A completed unit may publish another handler's wakes. Notify every
/// owning line directly, preserving the recorded deadlines. No HTTP kick
/// can fail between the durable write and this notification; failed units
/// are refreshed too, and startup recovers files left before a restart.
pub fn refresh(handlers: &Handlers, lines: &Lines) {
    for (name, h) in handlers.iter() {
        if let Some(wake) = &h.wake {
            lines[name].wakes(&booked(wake));
        }
    }
}

/// Start each of the handler's due passes, for as long as the service runs
/// (`once`: one round, for the tests).
pub fn serve(
    name: &str,
    h: &Handler,
    line: &Line,
    units: &impl Units,
    once: bool,
    refresh: impl Fn(),
) {
    if let Some(wake) = &h.wake {
        line.wakes(&booked(wake));
    }
    loop {
        let (ready, wait) = line.take(now());
        for (pass, scope) in &ready {
            let unit = h.unit(pass);
            match scope {
                Scope::All => eprintln!("{name}: {pass}"),
                Scope::Issues(ns) => {
                    let ns: Vec<String> = ns.iter().map(|n| format!("#{n}")).collect();
                    eprintln!("{name}: {pass} {}", ns.join(" "));
                }
            }
            // One already running (from before this service started, say)
            // began before this pass was due, and asking for it again only
            // waits for that one: so it runs again once that one is done.
            if units.state(&unit) == "activating" {
                units.start(&unit);
            }
            let told = tell(name, h, pass, scope);
            let ok = units.start(&unit);
            if let Some(f) = told {
                let _ = fs::remove_file(f);
            }
            if let Some(again) = line.done(pass, ok, now()) {
                eprintln!(
                    "{name}: {pass} failed; again in {}s",
                    (again - now()).round()
                );
            }
            refresh();
        }
        if once {
            return;
        }
        if ready.is_empty() {
            line.wait(wait);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};
    use std::fs;
    use std::path::PathBuf;

    use super::{Units, serve};
    use crate::handler::Handler;
    use crate::line::{Line, Pass};

    /// systemd, stood in for: which units were started, and a pass that
    /// books its next wake-up as it runs.
    struct Fake {
        wake: PathBuf,
        contents: String,
        started: RefCell<Vec<String>>,
        running: HashSet<String>,
        failing: HashSet<String>,
        /// Where the passes are told their issues: what each read there.
        told: Option<PathBuf>,
        seen: RefCell<Vec<(String, Option<String>)>>,
    }

    impl Units for Fake {
        fn state(&self, unit: &str) -> String {
            if self.running.contains(unit) {
                "activating"
            } else {
                "inactive"
            }
            .into()
        }

        fn start(&self, unit: &str) -> bool {
            self.started.borrow_mut().push(unit.into());
            if let Some(told) = &self.told {
                let instance = unit
                    .trim_start_matches("autoland@")
                    .trim_end_matches(".service");
                let read = fs::read_to_string(told.join(instance)).ok();
                self.seen.borrow_mut().push((unit.into(), read));
            }
            fs::write(&self.wake, &self.contents).unwrap();
            !self.failing.contains(unit)
        }
    }

    fn setup() -> (tempfile::TempDir, Handler, Fake) {
        let dir = tempfile::tempdir().unwrap();
        let wake = dir.path().join("wake.json");
        fs::write(&wake, r#"{"sam/w": 0}"#).unwrap();
        let h = Handler {
            unit: "autoland".into(),
            per_repo: true,
            wake: Some(wake.clone()),
            ..Handler::default()
        };
        let fake = Fake {
            wake,
            contents: r#"{"sam/next": 1}"#.into(),
            started: RefCell::default(),
            running: HashSet::new(),
            failing: HashSet::new(),
            told: None,
            seen: RefCell::default(),
        };
        (dir, h, fake)
    }

    // Each due pass starts its unit, the full one before the rest, and the
    // wake file is read when the service starts and after each pass.
    #[test]
    fn starts_and_wakes() {
        let (_dir, h, fake) = setup();
        let line = Line::default();
        line.event(Pass::Full, -100.0);
        serve("autoland", &h, &line, &fake, true, || {
            line.wakes(&super::booked(h.wake.as_ref().unwrap()));
        });
        assert_eq!(*fake.started.borrow(), ["autoland.service"]);
        assert_eq!(line.due(), HashMap::from([(Pass::repo("sam/next"), 1.0)]));
        serve("autoland", &h, &line, &fake, true, || {
            line.wakes(&super::booked(h.wake.as_ref().unwrap()));
        });
        assert_eq!(
            *fake.started.borrow(),
            ["autoland.service", "autoland@sam-next.service"]
        );
    }

    // A pass already running when its turn comes (from before a restart)
    // began before it was due: asking again only waits for it, so it runs
    // once more.
    #[test]
    fn running_runs_again() {
        let (_dir, h, mut fake) = setup();
        fake.running.insert("autoland.service".into());
        let line = Line::default();
        line.event(Pass::Full, -100.0);
        serve("autoland", &h, &line, &fake, true, || {
            line.wakes(&super::booked(h.wake.as_ref().unwrap()));
        });
        assert_eq!(
            *fake.started.borrow(),
            ["autoland.service", "autoland.service"]
        );
    }

    // A pass that failed (killed at its timeout, say) runs again a minute
    // later: nothing else may come for it.
    #[test]
    fn failed_runs_again() {
        let (_dir, h, mut fake) = setup();
        fake.failing.insert("autoland.service".into());
        let line = Line::default();
        line.event(Pass::Full, -100.0);
        let before = crate::now();
        serve("autoland", &h, &line, &fake, true, || {
            line.wakes(&super::booked(h.wake.as_ref().unwrap()));
        });
        let at = line.due()[&Pass::Full];
        assert!(before + 60.0 <= at && at <= crate::now() + 60.0, "{at}");
    }

    #[test]
    fn refreshes_each_completed_pass() {
        let (_dir, h, fake) = setup();
        let line = Line::default();
        line.event(Pass::repo("sam/a"), -100.0);
        line.event(Pass::repo("sam/b"), -100.0);
        let notifications = RefCell::new(Vec::new());
        serve("autoland", &h, &line, &fake, true, || {
            notifications.borrow_mut().push(fake.started.borrow().len());
        });
        assert_eq!(*notifications.borrow(), [1, 2]);
    }

    // Completing either event line delivers cross-handler publications to
    // an idle owner. Decision, unavailable and rate-limit deadlines retain
    // their times, including after a unit fails or the runner restarts.
    #[test]
    fn cross_handler_wakes() {
        use std::collections::BTreeMap;
        use std::sync::Arc;

        use serde_json::json;

        for (source, target) in [("ordinary", "continuation"), ("continuation", "ordinary")] {
            for failed in [false, true] {
                let (dir, mut target_handler, mut fake) = setup();
                let future = crate::now().ceil() + 3600.0;
                fake.contents = json!({
                    "sam/next": 1,
                    "sam/decision": future,
                    "sam/unavailable": future + 300.0,
                    "sam/limited": future + 600.0,
                })
                .to_string();
                target_handler.unit = target.into();
                let source_handler = Handler {
                    unit: source.into(),
                    wake: Some(dir.path().join("source.json")),
                    ..Handler::default()
                };
                if failed {
                    fake.failing.insert(format!("{source}.service"));
                }
                let handlers = Arc::new(BTreeMap::from([
                    (source.to_owned(), source_handler),
                    (target.to_owned(), target_handler),
                ]));
                let lines = Arc::new(BTreeMap::from([
                    (source.to_owned(), Line::default()),
                    (target.to_owned(), Line::default()),
                ]));
                assert!(lines[target].take(crate::now()).0.is_empty());
                lines[source].event(Pass::Full, -100.0);
                serve(
                    source,
                    &handlers[source],
                    &lines[source],
                    &fake,
                    true,
                    || {
                        super::refresh(&handlers, &lines);
                    },
                );
                assert_eq!(lines[target].due()[&Pass::repo("sam/decision")], future);
                assert_eq!(
                    lines[target].due()[&Pass::repo("sam/unavailable")],
                    future + 300.0
                );
                assert_eq!(
                    lines[target].due()[&Pass::repo("sam/limited")],
                    future + 600.0
                );
                assert_eq!(
                    lines[target]
                        .take(crate::now())
                        .0
                        .into_iter()
                        .map(|(p, _)| p)
                        .collect::<Vec<_>>(),
                    [Pass::repo("sam/next")]
                );

                // Durable files also recover a publication if the process
                // stops before notification. Startup needs no other event.
                let recovered = Line::default();
                fake.contents = json!({"sam/limited": future + 600.0}).to_string();
                fs::write(&fake.wake, &fake.contents).unwrap();
                serve(target, &handlers[target], &recovered, &fake, true, || {});
                assert_eq!(recovered.take(future + 599.0).0, []);
                assert_eq!(
                    recovered
                        .take(future + 600.0)
                        .0
                        .into_iter()
                        .map(|(p, _)| p)
                        .collect::<Vec<_>>(),
                    [Pass::repo("sam/limited")]
                );
            }
        }
    }

    // A wake file that's missing or isn't an object books nothing.
    #[test]
    fn odd_wake_files() {
        let dir = tempfile::tempdir().unwrap();
        assert!(super::booked(&dir.path().join("none")).is_empty());
        let odd = dir.path().join("odd.json");
        fs::write(&odd, "[1, 2]").unwrap();
        assert!(super::booked(&odd).is_empty());
        fs::write(&odd, "{not json").unwrap();
        assert!(super::booked(&odd).is_empty());
    }

    // A pass over a repo's issues finds their numbers in its file while it
    // runs, and is gone after; a look at everything finds none there,
    // whatever was left.
    #[test]
    fn tells_issues() {
        let (dir, mut h, mut fake) = setup();
        let told = dir.path().join("told");
        h.issue_dir = Some(told.clone());
        fake.told = Some(told.clone());
        let line = Line::new(0.0);
        line.issue(Pass::repo("sam/r"), 9, -100.0);
        line.issue(Pass::repo("Sam/R"), 7, -100.0);
        serve("autoland", &h, &line, &fake, true, || {});
        assert!(!told.join("sam-r").exists());
        fs::write(told.join("sam-r"), "3\n").unwrap();
        line.event(Pass::repo("sam/r"), -100.0);
        serve("autoland", &h, &line, &fake, true, || {});
        assert_eq!(
            *fake.seen.borrow(),
            [
                ("autoland@sam-r.service".into(), Some("7\n9\n".into())),
                ("autoland@sam-next.service".into(), None),
                ("autoland@sam-r.service".into(), None),
            ]
        );
        assert!(!told.join("sam-r").exists());
    }
}
