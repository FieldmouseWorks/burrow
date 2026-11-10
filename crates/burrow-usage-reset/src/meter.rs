//! The engines' usage meters, as role overflow reads them (proj's
//! engine::role): one file an engine in the meters dir, `claude.json` and
//! `codex.json`, each the engine's last fresh reading whole, and `log`, one
//! line a reading (`{"engine": ..., ...}`), for the history. Every limit
//! window an engine has counts (a five-hour session and a week each), and
//! so does paid usage that carries on past them (Codex's credits): an engine
//! with credits is never short, whatever its windows say.

use crate::pass::{Account, Reading, Reset};
use crate::store::write;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// How long a pass may book its next for at most, so the meters stay
/// fresh: a five-hour window can go from half used to used up in less than
/// the hour a week's burn would otherwise wait.
pub const METERED: f64 = 900.0;
/// How old a reading may be and still move a turn: the passes read at least
/// every [`METERED`], so an older one says they've stopped.
pub const FRESH: f64 = 3600.0;

/// One engine's meter: the meters dir's ENGINE.json, and the log's lines.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    /// Each limit window by the engine's name for it.
    pub windows: BTreeMap<String, Window>,
    /// Paid usage that carries on once a window is used up.
    pub credits: bool,
    /// When the reading completed, in seconds since the epoch.
    pub read_at: f64,
    /// Before the fresh reading was requested. Older producers did not keep
    /// this: their readings can route work but cannot prove recovery after
    /// a refusal while a request was in flight.
    #[serde(default)]
    pub requested_at: Option<f64>,
}

/// One limit window: percent used, 0 to 100, and when it resets by itself,
/// in seconds since the epoch.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub used: f64,
    pub resets_at: f64,
}

impl Snapshot {
    /// ENGINE's reading in DIR, if there's one fresh at NOW that makes
    /// sense. A missing, malformed, future or stale one is none: it moves
    /// nothing.
    pub fn read(dir: &Path, engine: &str, now: f64) -> Option<Self> {
        let bytes = fs::read(dir.join(format!("{engine}.json"))).ok()?;
        let s: Self = serde_json::from_slice(&bytes).ok()?;
        (s.read_at <= now
            && now - s.read_at < FRESH
            && s.windows.values().all(|w| (0.0..=100.0).contains(&w.used)))
        .then_some(s)
    }

    /// Short of usage at PCT: a window at or past it, and no credits to
    /// carry on with.
    pub fn short(&self, pct: f64) -> bool {
        !self.credits && self.windows.values().any(|w| w.used >= pct)
    }

    /// Room for a turn: credits, or no window used up.
    pub fn room(&self) -> bool {
        self.credits || self.windows.values().all(|w| w.used < 100.0)
    }

    /// Keep it as ENGINE's reading in DIR, and add it to the log.
    pub fn record(&self, dir: &Path, engine: &str) -> Result<(), String> {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let json = serde_json::to_value(self).map_err(|e| e.to_string())?;
        write(
            &dir.join(format!("{engine}.json")),
            format!("{json}\n").as_bytes(),
        )?;
        let mut line = serde_json::json!({ "engine": engine });
        if let (Some(line), serde_json::Value::Object(fields)) = (line.as_object_mut(), json) {
            line.extend(fields);
        }
        let log = dir.join("log");
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|e| format!("{}: {e}", log.display()))?;
        file.write_all(format!("{line}\n").as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("{}: {e}", log.display()))
    }
}

/// Claude's meter in a reading: its week, and its session while one runs.
pub fn claude(reading: &Reading, at: f64) -> Snapshot {
    let mut windows = BTreeMap::from([(
        "seven_day".to_owned(),
        Window {
            used: reading.used,
            resets_at: reading.ends,
        },
    )]);
    if let Some(session) = &reading.session {
        windows.insert("five_hour".to_owned(), session.clone());
    }
    Snapshot {
        windows,
        credits: false,
        read_at: at,
        requested_at: Some(at),
    }
}

/// An account whose every fresh reading is kept as Claude's meter in DIR;
/// keeping it never changes what the pass decides.
pub struct Meter<A> {
    pub account: A,
    pub dir: PathBuf,
}
impl<A: Account> Account for Meter<A> {
    fn read(&mut self) -> Result<Reading, String> {
        let started = crate::now();
        let reading = self.account.read()?;
        let mut snapshot = claude(&reading, crate::now());
        snapshot.requested_at = Some(started);
        if let Err(e) = snapshot.record(&self.dir, "claude") {
            eprintln!("couldn't record Claude's meter: {e}");
        }
        Ok(reading)
    }
    fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
        self.account.reset(keep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(used: f64) -> Window {
        Window {
            used,
            resets_at: 20000.0,
        }
    }

    fn snapshot(windows: &[(&str, f64)], credits: bool, read_at: f64) -> Snapshot {
        Snapshot {
            windows: windows
                .iter()
                .map(|(n, u)| ((*n).to_owned(), window(*u)))
                .collect(),
            credits,
            read_at,
            requested_at: Some(read_at),
        }
    }

    #[test]
    fn each_engine_keeps_its_last_reading_and_the_log_keeps_every_one() {
        let dir = tempfile::tempdir().unwrap();
        let reading = Reading {
            used: 81.0,
            week: "2026-11-07T12:00".into(),
            ends: 20000.0,
            session: Some(window(97.0)),
        };
        claude(&reading, 10000.0)
            .record(dir.path(), "claude")
            .unwrap();
        snapshot(&[("300m", 100.0)], true, 10001.0)
            .record(dir.path(), "codex")
            .unwrap();
        claude(
            &Reading {
                used: 82.0,
                session: None,
                ..reading
            },
            10002.0,
        )
        .record(dir.path(), "claude")
        .unwrap();
        let log = fs::read_to_string(dir.path().join("log")).unwrap();
        let lines: Vec<serde_json::Value> = log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0]["engine"], "claude");
        assert_eq!(lines[0]["windows"]["five_hour"]["used"], 97.0);
        assert_eq!(lines[0]["windows"]["seven_day"]["used"], 81.0);
        assert_eq!(lines[1]["engine"], "codex");
        assert_eq!(lines[1]["credits"], true);
        // The last reading stands alone: a session that ended is gone.
        let last = Snapshot::read(dir.path(), "claude", 10003.0).unwrap();
        assert_eq!(last, snapshot(&[("seven_day", 82.0)], false, 10002.0));
        assert!(!dir.path().join("claude.tmp").exists());
    }

    #[test]
    fn only_a_fresh_sensible_reading_counts() {
        let dir = tempfile::tempdir().unwrap();
        let at = |s: Snapshot| {
            s.record(dir.path(), "claude").unwrap();
            Snapshot::read(dir.path(), "claude", 10000.0)
        };
        assert!(Snapshot::read(dir.path(), "claude", 10000.0).is_none());
        assert!(at(snapshot(&[("seven_day", 50.0)], false, 9000.0)).is_some());
        // An hour old, from the future, or past 100%.
        assert!(at(snapshot(&[("seven_day", 50.0)], false, 10000.0 - FRESH)).is_none());
        assert!(at(snapshot(&[("seven_day", 50.0)], false, 10001.0)).is_none());
        assert!(at(snapshot(&[("seven_day", 101.0)], false, 9000.0)).is_none());
        // Another program's file, or half of one.
        fs::write(dir.path().join("claude.json"), r#"{"utilization": 50.0}"#).unwrap();
        assert!(Snapshot::read(dir.path(), "claude", 10000.0).is_none());
    }

    #[test]
    fn short_at_any_window_unless_it_has_credits_and_room_until_one_is_used_up() {
        let both = snapshot(&[("five_hour", 96.0), ("seven_day", 40.0)], false, 0.0);
        assert!(both.short(95.0));
        assert!(!both.short(97.0));
        assert!(both.room());
        let out = snapshot(&[("300m", 100.0), ("10080m", 60.0)], false, 0.0);
        assert!(out.short(95.0) && !out.room());
        // Credits carry it past every window.
        let paid = Snapshot {
            credits: true,
            ..out
        };
        assert!(!paid.short(95.0) && paid.room());
    }

    #[test]
    fn meter_timestamp_precedes_the_request_even_when_the_response_is_delayed() {
        struct Delayed {
            observed: f64,
        }
        impl Account for Delayed {
            fn read(&mut self) -> Result<Reading, String> {
                self.observed = crate::now();
                std::thread::sleep(std::time::Duration::from_millis(20));
                Ok(Reading {
                    used: 1.0,
                    week: "week".into(),
                    ends: 20000.0,
                    session: None,
                })
            }
            fn reset(&mut self, _: &mut dyn FnMut() -> Result<(), String>) -> Reset {
                unreachable!()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut metered = Meter {
            account: Delayed { observed: 0.0 },
            dir: dir.path().into(),
        };
        metered.read().unwrap();
        let snapshot = Snapshot::read(dir.path(), "claude", crate::now()).unwrap();
        assert!(
            snapshot.requested_at.unwrap() <= metered.account.observed,
            "a delayed response must not postdate an intervening refusal"
        );
    }
}
