//! Starting a handler's passes as they come due.

use std::collections::BTreeMap;
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
/// A line held off by another (either side of a `yieldTo`) hears each pass
/// end too, so it looks again once that one is idle.
pub fn refresh(handlers: &Handlers, lines: &Lines) {
    for (name, h) in handlers.iter() {
        if let Some(wake) = &h.wake {
            lines[name].wakes(&booked(wake));
        } else if h.yield_to.is_some() || !yielders(handlers, name).is_empty() {
            lines[name].nudge();
        }
    }
}

/// The handlers that yield to `name` (`yieldTo`): their running passes
/// hold its own off, since they share its lock.
pub fn yielders<'a>(handlers: &'a BTreeMap<String, Handler>, name: &str) -> Vec<&'a str> {
    handlers
        .iter()
        .filter(|(_, h)| h.yield_to.as_deref() == Some(name))
        .map(|(n, _)| n.as_str())
        .collect()
}

/// Each handler that yields (`yieldTo`) yields to another handler that
/// yields to none itself, so no two hold each other off.
pub fn check_yields(handlers: &BTreeMap<String, Handler>) -> Result<(), String> {
    for (name, h) in handlers {
        let Some(first) = &h.yield_to else { continue };
        match handlers.get(first) {
            Some(f) if first != name && f.yield_to.is_none() => {}
            Some(_) => return Err(format!("{name} yields to {first}, which yields itself")),
            None => return Err(format!("{name} yields to {first}, which isn't a handler")),
        }
    }
    Ok(())
}

/// What holds a handler's passes off: `first`, the line it yields to, while
/// that one has a pass running or due, and each of `behind`, the lines
/// yielding to it, while one of theirs runs. They share a lock, and a pass
/// started beside one that holds it would only come back every GAP until
/// that one ended. A pass of `behind`'s merely due holds nothing off, so
/// neither waits for the other for good.
#[derive(Default)]
pub struct Yields<'a> {
    pub first: Option<&'a Line>,
    pub behind: Vec<&'a Line>,
}

impl<'a> Yields<'a> {
    /// The handler `name`'s, among `lines`.
    pub fn of(
        handlers: &BTreeMap<String, Handler>,
        lines: &'a BTreeMap<String, Line>,
        name: &str,
    ) -> Self {
        Yields {
            first: handlers[name].yield_to.as_ref().map(|f| &lines[f]),
            behind: yielders(handlers, name)
                .iter()
                .map(|y| &lines[*y])
                .collect(),
        }
    }

    fn held(&self) -> bool {
        self.first.is_some_and(|f| f.busy(now())) || self.behind.iter().any(|b| b.running())
    }
}

/// Start each of the handler's due passes, for as long as the service runs
/// (`once`: one round, for the tests), none while `yields` holds them off.
pub fn serve(
    name: &str,
    h: &Handler,
    line: &Line,
    yields: &Yields,
    units: &impl Units,
    once: bool,
    refresh: impl Fn(),
) {
    if let Some(wake) = &h.wake {
        line.wakes(&booked(wake));
    }
    loop {
        if once && yields.held() {
            return;
        }
        if !once && line.hold(|| yields.held()) {
            continue;
        }
        let (ready, wait) = line.take(now());
        let started = !ready.is_empty();
        let mut ready = ready.into_iter();
        while let Some((pass, scope)) = ready.next() {
            let unit = h.unit(&pass);
            // One already running (from before this service started, say)
            // began before this pass was due, and asking for it again only
            // waits for that one: so it runs again once that one is done.
            if units.state(&unit) == "activating" {
                units.start(&unit);
            }
            // Yielding is at every pass's start: the line it yields to may
            // have got work while the one before ran (or one yielding to it
            // started a pass). What hasn't started goes back, as it was, for
            // once that line is idle.
            if yields.held() {
                let at = now();
                line.untake(&pass, &scope, at);
                for (pass, scope) in ready.by_ref() {
                    line.untake(&pass, &scope, at);
                }
                break;
            }
            match &scope {
                Scope::All => eprintln!("{name}: {pass}"),
                Scope::Issues(ns) => {
                    let ns: Vec<String> = ns.iter().map(|n| format!("#{n}")).collect();
                    eprintln!("{name}: {pass} {}", ns.join(" "));
                }
            }
            let told = tell(name, h, &pass, &scope);
            let ok = units.start(&unit);
            if let Some(f) = told {
                let _ = fs::remove_file(f);
            }
            if let Some(again) = line.done(&pass, ok, now()) {
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
        if !started {
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

    use super::{Units, Yields, serve};
    use crate::handler::Handler;
    use crate::line::{Line, Pass};

    /// A handler that yields to none, and none to it.
    const ALONE: Yields<'static> = Yields {
        first: None,
        behind: Vec::new(),
    };

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
        /// What else happens as a unit starts.
        hook: Option<Hook>,
    }

    type Hook = Box<dyn Fn(&str)>;

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
            if let Some(hook) = &self.hook {
                hook(unit);
            }
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
            hook: None,
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
        serve("autoland", &h, &line, &ALONE, &fake, true, || {
            line.wakes(&super::booked(h.wake.as_ref().unwrap()));
        });
        assert_eq!(*fake.started.borrow(), ["autoland.service"]);
        assert_eq!(line.due(), HashMap::from([(Pass::repo("sam/next"), 1.0)]));
        serve("autoland", &h, &line, &ALONE, &fake, true, || {
            line.wakes(&super::booked(h.wake.as_ref().unwrap()));
        });
        assert_eq!(
            *fake.started.borrow(),
            ["autoland.service", "autoland@sam-next.service"]
        );
    }

    // A repo's pass due with the full one starts first, and the full pass
    // starts too: the steward's full pass yields after a slice, so it never
    // stands in for a repo's.
    #[test]
    fn repo_beside_full() {
        let (_dir, h, fake) = setup();
        let line = Line::default();
        line.event(Pass::Full, -100.0);
        line.event(Pass::repo("FieldmouseWorks/Conary"), -100.0);
        serve("autoland", &h, &line, &ALONE, &fake, true, || {});
        assert_eq!(
            *fake.started.borrow(),
            [
                "autoland@fieldmouseworks-conary.service",
                "autoland.service"
            ]
        );
    }

    // A handler that yields to another starts nothing while that one has a
    // pass due or running, and its own pass once that one is idle.
    #[test]
    fn yields() {
        let (_dir, mut h, fake) = setup();
        h.unit = "autoland-continue".into();
        let (first, line) = (Line::default(), Line::default());
        line.event(Pass::Full, -100.0);
        first.event(Pass::repo("sam/r"), -100.0);
        let yields = Yields {
            first: Some(&first),
            ..ALONE
        };
        serve("continue", &h, &line, &yields, &fake, true, || {});
        assert!(fake.started.borrow().is_empty());
        let (ready, _) = first.take(crate::now());
        assert_eq!(ready.len(), 1);
        serve("continue", &h, &line, &yields, &fake, true, || {});
        assert!(fake.started.borrow().is_empty(), "its pass is running");
        first.done(&Pass::repo("sam/r"), true, crate::now());
        serve("continue", &h, &line, &yields, &fake, true, || {});
        assert_eq!(*fake.started.borrow(), ["autoland-continue.service"]);
    }

    // Yielding is at every pass's start: work the line it yields to got
    // while one pass ran holds off the rest of the take, which goes back as
    // it was (an issue's look stays one) and starts once that line is idle.
    #[test]
    fn yields_between_passes() {
        use std::sync::Arc;

        let (_dir, mut h, mut fake) = setup();
        h.unit = "autoland-continue".into();
        h.wake = None;
        let first = Arc::new(Line::default());
        let line = Line::default();
        line.event(Pass::Full, -100.0);
        line.event(Pass::repo("sam/a"), -100.0);
        line.issue(Pass::repo("sam/b"), 7, -100.0);
        let kicked = first.clone();
        let yields = Yields {
            first: Some(&first),
            ..ALONE
        };
        fake.hook = Some(Box::new(move |unit| {
            if unit.contains("sam-a") {
                kicked.event(Pass::repo("sam/r"), -100.0);
            }
        }));
        serve("continue", &h, &line, &yields, &fake, true, || {});
        assert_eq!(*fake.started.borrow(), ["autoland-continue@sam-a.service"]);
        assert!(line.due().contains_key(&Pass::Full));
        assert_eq!(line.issues()[&Pass::repo("sam/b")].1, [7].into());
        serve("continue", &h, &line, &yields, &fake, true, || {});
        assert_eq!(fake.started.borrow().len(), 1, "still held off");
        let (ready, _) = first.take(crate::now());
        for (pass, _) in ready {
            first.done(&pass, true, crate::now());
        }
        serve("continue", &h, &line, &yields, &fake, true, || {});
        assert_eq!(
            *fake.started.borrow(),
            [
                "autoland-continue@sam-a.service",
                "autoland-continue@sam-b.service",
                "autoland-continue.service"
            ]
        );
        assert!(!line.busy(crate::now()), "every pass taken is done");
    }

    // The handler yielded to waits out a yielder's pass that runs, which
    // holds their shared lock, rather than start one that would only find
    // it held and come back every GAP; a yielder's pass merely due holds it
    // off not at all.
    #[test]
    fn waits_out_a_yielder() {
        let (_dir, h, fake) = setup();
        let (line, behind) = (Line::default(), Line::default());
        line.event(Pass::Full, -100.0);
        behind.event(Pass::Full, -100.0);
        let (ready, _) = behind.take(crate::now());
        assert_eq!(ready.len(), 1);
        let yields = Yields {
            behind: vec![&behind],
            ..ALONE
        };
        serve("autoland", &h, &line, &yields, &fake, true, || {});
        assert!(fake.started.borrow().is_empty(), "a continuation runs");
        behind.done(&Pass::Full, true, crate::now());
        behind.event(Pass::Full, -100.0);
        serve("autoland", &h, &line, &yields, &fake, true, || {});
        assert_eq!(*fake.started.borrow(), ["autoland.service"]);
    }

    // Yielding goes one step, to a handler there is: none holds another off
    // for good.
    #[test]
    fn checked_yields() {
        use std::collections::BTreeMap;

        let with = |pairs: &[(&str, Option<&str>)]| -> BTreeMap<String, Handler> {
            pairs
                .iter()
                .map(|(n, y)| {
                    let h = Handler {
                        yield_to: y.map(str::to_owned),
                        ..Handler::default()
                    };
                    (n.to_string(), h)
                })
                .collect()
        };
        assert!(super::check_yields(&with(&[("a", None), ("b", Some("a"))])).is_ok());
        for bad in [
            with(&[("a", Some("a"))]),
            with(&[("a", Some("b")), ("b", Some("a"))]),
            with(&[("a", Some("none"))]),
        ] {
            assert!(super::check_yields(&bad).is_err(), "{bad:?}");
        }
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
        serve("autoland", &h, &line, &ALONE, &fake, true, || {
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
        serve("autoland", &h, &line, &ALONE, &fake, true, || {
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
        serve("autoland", &h, &line, &ALONE, &fake, true, || {
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
                    &ALONE,
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
                serve(
                    target,
                    &handlers[target],
                    &recovered,
                    &ALONE,
                    &fake,
                    true,
                    || {},
                );
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
        serve("autoland", &h, &line, &ALONE, &fake, true, || {});
        assert!(!told.join("sam-r").exists());
        fs::write(told.join("sam-r"), "3\n").unwrap();
        line.event(Pass::repo("sam/r"), -100.0);
        serve("autoland", &h, &line, &ALONE, &fake, true, || {});
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
