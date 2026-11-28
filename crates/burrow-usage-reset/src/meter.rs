//! The engines' usage meters, as proj and the probe read them: one file an
//! engine in the meters dir (`claude.json`), its last fresh reading whole,
//! and `log`, one line a reading (`{"engine": ..., ...}`), for the history.
//! Every limit window an engine has counts (a five-hour session and a week),
//! and so does paid usage that carries on past them: an engine with credits
//! is never short, whatever its windows say.

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

/// Why an engine's reading can't move a turn: there's none, it can't be
/// read or parsed, a window's percent makes no sense, or it's from the
/// future or older than [`FRESH`].
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "unfit", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Unfit {
    Missing,
    Unreadable { error: String },
    Malformed { error: String },
    OutOfRange { window: String, used: f64 },
    Future { read_at: f64 },
    Stale { read_at: f64 },
}

/// One limit window: percent used, 0 to 100, and when it resets by itself,
/// in seconds since the epoch.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Window {
    pub used: f64,
    pub resets_at: f64,
}

/// How long a limit window runs, whatever an engine calls it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    FiveHour,
    Week,
}

impl Span {
    /// The span of a window by its engine's name for it: Claude's
    /// (`five_hour`, `seven_day`, as [`claude`] keeps them); None for one
    /// of another length.
    pub fn of(name: &str) -> Option<Span> {
        match name {
            "five_hour" => Some(Span::FiveHour),
            "seven_day" => Some(Span::Week),
            _ => None,
        }
    }
}

impl Snapshot {
    /// ENGINE's reading in DIR, if there's one fresh at NOW that makes
    /// sense. A missing, malformed, future or stale one is none: it moves
    /// nothing.
    pub fn read(dir: &Path, engine: &str, now: f64) -> Option<Self> {
        Self::check(dir, engine, now).ok()
    }

    /// ENGINE's reading in DIR if it's fresh at NOW and makes sense, or
    /// why it isn't: the evidence a usage selection keeps for each engine.
    pub fn check(dir: &Path, engine: &str, now: f64) -> Result<Self, Unfit> {
        let path = dir.join(format!("{engine}.json"));
        let bytes = fs::read(&path).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => Unfit::Missing,
            _ => Unfit::Unreadable {
                error: format!("{}: {e}", path.display()),
            },
        })?;
        let s: Self = serde_json::from_slice(&bytes).map_err(|e| Unfit::Malformed {
            error: e.to_string(),
        })?;
        if !s.read_at.is_finite() {
            return Err(Unfit::Malformed {
                error: "read_at isn't a time".into(),
            });
        }
        if let Some((name, w)) = s
            .windows
            .iter()
            .find(|(_, w)| !(0.0..=100.0).contains(&w.used))
        {
            return Err(Unfit::OutOfRange {
                window: name.clone(),
                used: w.used,
            });
        }
        if s.read_at > now {
            return Err(Unfit::Future { read_at: s.read_at });
        }
        if now - s.read_at >= FRESH {
            return Err(Unfit::Stale { read_at: s.read_at });
        }
        Ok(s)
    }

    /// ENGINE's reading in DIR as it was written, however old: for showing
    /// it (proj's live snapshot), never for moving a turn ([`Self::read`]).
    pub fn load(dir: &Path, engine: &str) -> Option<Self> {
        let bytes = fs::read(dir.join(format!("{engine}.json"))).ok()?;
        serde_json::from_slice(&bytes).ok()
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

    /// As a weekly reading for the usage watcher, when `seven_day` is there
    /// and its reset time formats as a UTC timestamp.
    pub fn reading(&self) -> Option<Reading> {
        let week_window = self.windows.get("seven_day")?;
        let ends = week_window.resets_at;
        let ts = jiff::Timestamp::from_second(ends.round() as i64).ok()?;
        Some(Reading {
            used: week_window.used,
            week: ts.to_string().chars().take(16).collect(),
            ends,
            session: self.windows.get("five_hour").cloned(),
        })
    }

    /// When this reading was taken, for ordering and age: before the request
    /// started when known, never after completion.
    fn observed_at(&self) -> f64 {
        match self.requested_at {
            Some(req) if req.is_finite() => req.min(self.read_at),
            _ => self.read_at,
        }
    }

    /// Whether every window percent is within 0..=100 and `read_at` is finite.
    fn valid_for_ordering(&self) -> bool {
        self.read_at.is_finite()
            && self
                .windows
                .values()
                .all(|w| (0.0..=100.0).contains(&w.used))
    }

    /// Whether `self` is an older reading than `other` (requested earlier, or
    /// requested at the same instant and completed earlier).
    fn is_older_than(&self, other: &Self) -> bool {
        if !other.valid_for_ordering() {
            return false;
        }
        if !self.read_at.is_finite() {
            return true;
        }
        let (a, b) = (self.observed_at(), other.observed_at());
        a < b || (a == b && self.read_at < other.read_at)
    }

    /// Keep it as ENGINE's reading in DIR when it is not older than the
    /// snapshot already there, and add it to the log.
    pub fn record(&self, dir: &Path, engine: &str) -> Result<(), String> {
        self.record_if_newer(dir, engine).map(|_| ())
    }

    /// Record this reading, returning whether `{engine}.json` was updated
    /// (`false` when an already-recorded reading was requested later).
    fn record_if_newer(&self, dir: &Path, engine: &str) -> Result<bool, String> {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let lock_path = dir.join(format!(".{engine}.lock"));
        let lock = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&lock_path)
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        lock.lock()
            .map_err(|e| format!("{}: {e}", lock_path.display()))?;
        let path = dir.join(format!("{engine}.json"));
        let existing = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Self>(&bytes).ok(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let json = serde_json::to_value(self).map_err(|e| e.to_string())?;
        let replaced = !existing.as_ref().is_some_and(|old| self.is_older_than(old));
        if replaced {
            write(&path, format!("{json}\n").as_bytes())?;
        }
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
            .map_err(|e| format!("{}: {e}", log.display()))?;
        Ok(replaced)
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
    pub inner: A,
    pub dir: PathBuf,
    /// When `read()` last recorded `claude.json` in this pass.
    pub recorded_at: Option<f64>,
}
impl<A: Account> Account for Meter<A> {
    fn read(&mut self) -> Result<Reading, String> {
        let started = crate::now();
        let reading = self.inner.read()?;
        let at = crate::now();
        let mut snapshot = claude(&reading, at);
        snapshot.requested_at = Some(started);
        match snapshot.record_if_newer(&self.dir, "claude") {
            Err(e) => eprintln!("couldn't record Claude's meter: {e}"),
            Ok(true) => self.recorded_at = Some(at),
            Ok(false) => {}
        }
        Ok(reading)
    }
    fn recorded(&mut self) -> Option<(f64, Reading)> {
        let snapshot = Snapshot::read(&self.dir, "claude", crate::now())?;
        Some((snapshot.observed_at(), snapshot.reading()?))
    }
    /// A copy isn't kept: nothing says when it was asked for, so it can't
    /// show proj a refusal over, and it would take the place of a reading
    /// that can.
    fn copy(&mut self) -> Option<(f64, Reading)> {
        self.inner.copy()
    }
    fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
        self.inner.reset(keep)
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
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["engine"], "claude");
        assert_eq!(lines[0]["windows"]["five_hour"]["used"], 97.0);
        assert_eq!(lines[0]["windows"]["seven_day"]["used"], 81.0);
        assert_eq!(lines[1]["engine"], "claude");
        // The last reading stands alone: a session that ended is gone.
        let last = Snapshot::read(dir.path(), "claude", 10003.0).unwrap();
        assert_eq!(last, snapshot(&[("seven_day", 82.0)], false, 10002.0));
        assert!(!dir.path().join("claude.tmp").exists());
    }

    #[test]
    fn only_a_fresh_sensible_reading_counts() {
        let dir = tempfile::tempdir().unwrap();
        let at = |s: Snapshot| {
            let _ = fs::remove_file(dir.path().join("claude.json"));
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
        let out = snapshot(&[("five_hour", 100.0), ("seven_day", 60.0)], false, 0.0);
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
            fn copy(&mut self) -> Option<(f64, Reading)> {
                None
            }
            fn reset(&mut self, _: &mut dyn FnMut() -> Result<(), String>) -> Reset {
                unreachable!()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let mut metered = Meter {
            inner: Delayed { observed: 0.0 },
            dir: dir.path().into(),
            recorded_at: None,
        };
        metered.read().unwrap();
        let snapshot = Snapshot::read(dir.path(), "claude", crate::now()).unwrap();
        assert_eq!(metered.recorded_at, Some(snapshot.read_at));
        assert!(
            snapshot.requested_at.unwrap() <= metered.inner.observed,
            "a delayed response must not postdate an intervening refusal"
        );
    }

    #[test]
    fn recorded_reads_back_a_fresh_shared_meter() {
        struct Uncalled;
        impl Account for Uncalled {
            fn read(&mut self) -> Result<Reading, String> {
                panic!("should not read live")
            }
            fn copy(&mut self) -> Option<(f64, Reading)> {
                None
            }
            fn reset(&mut self, _: &mut dyn FnMut() -> Result<(), String>) -> Reset {
                unreachable!()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let reading = Reading {
            used: 12.5,
            week: "2026-11-04T19:00".into(),
            ends: 1_793_818_800.0,
            session: Some(window(25.0)),
        };
        let at = crate::now() - 10.0;
        claude(&reading, at).record(dir.path(), "claude").unwrap();
        let mut metered = Meter {
            inner: Uncalled,
            dir: dir.path().into(),
            recorded_at: None,
        };
        assert_eq!(metered.recorded(), Some((at, reading)));
    }

    #[test]
    fn snapshot_timestamps_round_trip_without_ulp_drift() {
        let dir = tempfile::tempdir().unwrap();
        // Exact epoch timestamp that drifted by 1 ULP without serde_json's
        // float_roundtrip feature (1795712392.5840235 -> 1795712392.5840237).
        let at = 1_795_712_392.584_023_5_f64;
        let expected = snapshot(&[("seven_day", 12.5)], false, at);
        expected.record(dir.path(), "claude").unwrap();
        let read_back = Snapshot::read(dir.path(), "claude", at + 10.0).unwrap();
        assert_eq!(read_back, expected);
    }

    #[test]
    fn slower_earlier_request_cannot_overwrite_newer_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let now = crate::now();
        // Request B started later (now - 7s) and completed first (now - 6s)
        // with higher utilization (95%).
        let newer = Snapshot {
            requested_at: Some(now - 7.0),
            ..snapshot(&[("seven_day", 95.0)], false, now - 6.0)
        };
        newer.record(dir.path(), "claude").unwrap();
        // Request A started earlier (now - 10s) and finished later (now - 0.5s,
        // e.g. waiting on EXIT_GRACE) with stale utilization (80%).
        let slower_older = Snapshot {
            requested_at: Some(now - 10.0),
            ..snapshot(&[("seven_day", 80.0)], false, now - 0.5)
        };
        slower_older.record(dir.path(), "claude").unwrap();

        // claude.json keeps the newer request, while log keeps both for history.
        let kept = Snapshot::read(dir.path(), "claude", now).unwrap();
        assert_eq!(kept, newer);
        let log = fs::read_to_string(dir.path().join("log")).unwrap();
        assert_eq!(log.lines().count(), 2);

        // Concurrent writers across threads always leave the newest request.
        let conc = tempfile::tempdir().unwrap();
        std::thread::scope(|scope| {
            for i in 0..16 {
                let dir = conc.path();
                scope.spawn(move || {
                    let s = Snapshot {
                        requested_at: Some(1000.0 + i as f64),
                        // Deliberately invert completion time relative to request time.
                        ..snapshot(&[("seven_day", i as f64)], false, 2000.0 - i as f64)
                    };
                    s.record(dir, "claude").unwrap();
                });
            }
        });
        let latest = Snapshot::read(conc.path(), "claude", 2100.0).unwrap();
        assert_eq!(latest.requested_at, Some(1015.0));
        assert_eq!(latest.windows["seven_day"].used, 15.0);
    }

    #[test]
    fn recorded_uses_request_time_and_read_preserves_newer_shared_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let now = crate::now();
        // A reading requested 20s ago that finished 1s ago reports its request
        // time, not its delayed completion time, so pass() cannot treat it as
        // newer than an intervening reading.
        let delayed = Snapshot {
            requested_at: Some(now - 20.0),
            ..snapshot(&[("seven_day", 40.0)], false, now - 1.0)
        };
        delayed.record(dir.path(), "claude").unwrap();

        struct Interleaved {
            dir: PathBuf,
        }
        impl Account for Interleaved {
            fn read(&mut self) -> Result<Reading, String> {
                // While this read is in flight, another process starts later
                // and records a newer snapshot first.
                let later = crate::now() + 0.05;
                let newer = Snapshot {
                    requested_at: Some(later),
                    ..snapshot(&[("seven_day", 90.0)], false, later)
                };
                newer.record(&self.dir, "claude").unwrap();
                Ok(Reading {
                    used: 50.0,
                    week: "2026-11-04T19:00".into(),
                    ends: 20000.0,
                    session: None,
                })
            }
            fn copy(&mut self) -> Option<(f64, Reading)> {
                None
            }
            fn reset(&mut self, _: &mut dyn FnMut() -> Result<(), String>) -> Reset {
                unreachable!()
            }
        }

        let mut metered = Meter {
            inner: Interleaved {
                dir: dir.path().into(),
            },
            dir: dir.path().into(),
            recorded_at: None,
        };
        let (recorded_at, reading) = metered.recorded().unwrap();
        assert_eq!(recorded_at, now - 20.0);
        assert_eq!(reading.used, 40.0);

        metered.read().unwrap();
        assert_eq!(
            metered.recorded_at, None,
            "must not advance recorded_at when a newer snapshot was kept"
        );
        let kept = Snapshot::read(dir.path(), "claude", crate::now() + 1.0).unwrap();
        assert_eq!(kept.windows["seven_day"].used, 90.0);
    }
}
