//! Rolling baselines and anomalies: the policy the watchdog applies to the
//! numbers its sources already observe, with no model and no collector of
//! its own.
//!
//! Sources drop observations (`{"observations": [{check, subject, value, at,
//! link}]}`) in the files `anomalies.inputs` names. A pass folds them into a
//! bounded history per check-and-subject (anomalies.json in the state
//! directory), judges each new sample against the median of the healthy
//! samples before it, and writes the anomalies that are active as a watch
//! file. That file is a source like any other, so an anomaly takes the
//! ladder's one notice lifecycle (kick, one job, stuck issue, owner) the
//! degradations reader's items take, and the ladder's stall for it is named
//! by the anomaly's key and the time its breach began: stable across passes
//! and restarts.
//!
//! - Cold start: a subject with fewer than `minSamples` healthy samples
//!   behind a sample is recorded but never judged.
//! - Sustained, not transient: a breach is an anomaly once it has held for
//!   `minBreachSeconds`; one healthy sample ends a breach still short of it.
//! - An active anomaly clears after `clearSeconds` of healthy samples; a
//!   breach in between starts that count again, so a flapping value is one
//!   anomaly, not a job a flap.
//! - Duplicate observations (same subject and time) are ignored; late ones
//!   (older than the newest judged) enter the history but never move the
//!   breach state, which only runs forward.
//! - Missing data is no news: a subject nobody observed keeps its state, an
//!   active anomaly stays active, and an unreadable input changes nothing.
//! - Dismissal ([`State::dismiss`]) holds an active anomaly out of the watch
//!   file, so its notice closes, until it clears: the same evidence going on
//!   doesn't open another.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::{Anomalies, Mode, Rule};
use crate::watch::{File, Item};

/// One number a source observed.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Observation {
    pub check: String,
    pub subject: String,
    pub value: f64,
    pub at: f64,
    #[serde(default)]
    pub link: Option<String>,
}

/// An observation as history keeps it.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Sample {
    pub at: f64,
    pub value: f64,
    /// It breached its rule: left out of the baseline.
    #[serde(default)]
    pub breach: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// What an anomaly was seen on: frozen when it began.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Evidence {
    pub observed: f64,
    pub baseline: f64,
    pub from: f64,
    pub to: f64,
    pub links: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct Track {
    pub history: Vec<Sample>,
    /// The newest sample judged.
    pub judged: f64,
    /// When the current breach began.
    pub breach_since: Option<f64>,
    /// When the healthy run that may clear an active anomaly began.
    pub healthy_since: Option<f64>,
    /// Active since (its breach's start), with the evidence it began on.
    pub active: Option<(f64, Evidence)>,
    pub dismissed: bool,
}

/// anomalies.json.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct State {
    pub tracks: BTreeMap<String, Track>,
}

/// An anomaly's stable name: its check and subject.
pub fn key(check: &str, subject: &str) -> String {
    format!("{check}/{subject}")
}

impl State {
    /// Hold the active anomaly of CHECK and SUBJECT out of the watch file
    /// until it clears. False when there is none.
    pub fn dismiss(&mut self, check: &str, subject: &str) -> bool {
        match self.tracks.get_mut(&key(check, subject)) {
            Some(t) if t.active.is_some() => {
                t.dismissed = true;
                true
            }
            _ => false,
        }
    }

    /// Fold observations in, judging each subject's new samples in time order.
    pub fn observe(&mut self, rules: &Anomalies, mut obs: Vec<Observation>, now: f64) {
        obs.retain(|o| {
            rules.rules.contains_key(&o.check)
                && o.value.is_finite()
                && o.at.is_finite()
                && o.at <= now + 60.0
                && !o.subject.is_empty()
                && !o.check.contains(['/', '\t', '\n'])
                && !o.subject.contains(['\t', '\n'])
        });
        obs.sort_by(|a, b| a.at.total_cmp(&b.at));
        for o in obs {
            let rule = &rules.rules[&o.check];
            let t = self.tracks.entry(key(&o.check, &o.subject)).or_default();
            if t.history.iter().any(|s| s.at == o.at) {
                continue;
            }
            let late = o.at <= t.judged && !t.history.is_empty();
            let pos = t.history.partition_point(|s| s.at < o.at);
            t.history.insert(
                pos,
                Sample {
                    at: o.at,
                    value: o.value,
                    breach: false,
                    link: o.link.clone(),
                },
            );
            if late {
                // History only: the breach state runs forward.
                continue;
            }
            t.judge(rules, rule, pos);
        }
        self.retain(rules, now);
    }

    /// Bounded history, and no track left with nothing in it.
    fn retain(&mut self, rules: &Anomalies, now: f64) {
        for t in self.tracks.values_mut() {
            t.history.retain(|s| s.at >= now - rules.retention_seconds);
            let extra = t.history.len().saturating_sub(rules.retention_samples);
            t.history.drain(..extra);
        }
        self.tracks
            .retain(|_, t| !t.history.is_empty() || t.active.is_some());
    }

    /// The active, undismissed anomalies as a watch file.
    pub fn watch(&self, now: f64) -> File {
        let items = self
            .tracks
            .iter()
            .filter(|(_, t)| !t.dismissed)
            .filter_map(|(k, t)| {
                let (since, ev) = t.active.as_ref()?;
                let item = Item {
                    what: format!(
                        "anomaly {k}: observed {} against a baseline of {} between {} and {}{}",
                        ev.observed,
                        ev.baseline,
                        ev.from,
                        ev.to,
                        ev.links.iter().map(|l| format!(" {l}")).collect::<String>(),
                    ),
                    phase: "sustained anomaly".into(),
                    since: *since,
                    due: *since,
                    link: ev.links.last().cloned(),
                    ..Item::default()
                };
                Some((k.clone(), item))
            })
            .collect();
        File { at: now, items }
    }
}

impl Track {
    fn judge(&mut self, all: &Anomalies, rule: &Rule, pos: usize) {
        let s = self.history[pos].clone();
        self.judged = s.at;
        let base: Vec<f64> = self.history[..pos]
            .iter()
            .filter(|p| !p.breach)
            .map(|p| p.value)
            .collect();
        if base.len() < rule.min_samples {
            // Cold: recorded, never judged.
            return;
        }
        let baseline = median(&base);
        let breach = match rule.mode {
            Mode::Ratio => s.value >= baseline * rule.threshold,
            Mode::Above => s.value >= rule.threshold,
            Mode::Below => s.value <= rule.threshold,
        };
        self.history[pos].breach = breach;
        if breach {
            self.healthy_since = None;
            let since = *self.breach_since.get_or_insert(s.at);
            if self.active.is_none() && s.at - since >= rule.min_breach_seconds {
                let links = self
                    .history
                    .iter()
                    .filter(|p| p.at >= since && p.breach)
                    .filter_map(|p| p.link.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .take(all.max_links)
                    .collect();
                self.active = Some((
                    since,
                    Evidence {
                        observed: s.value,
                        baseline,
                        from: since,
                        to: s.at,
                        links,
                    },
                ));
            }
        } else if self.active.is_some() {
            let from = *self.healthy_since.get_or_insert(s.at);
            if s.at - from >= rule.clear_seconds {
                self.active = None;
                self.breach_since = None;
                self.healthy_since = None;
                self.dismissed = false;
            }
        } else {
            self.breach_since = None;
        }
    }
}

fn median(v: &[f64]) -> f64 {
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// The observations the input files hold; a missing or unreadable one is
/// missing data, which changes nothing.
pub fn read(inputs: &[std::path::PathBuf]) -> Vec<Observation> {
    #[derive(Deserialize)]
    struct Input {
        observations: Vec<Observation>,
    }
    inputs
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .filter_map(|b| serde_json::from_slice::<Input>(&b).ok())
        .flat_map(|i| i.observations)
        .collect()
}

/// One pass: inputs in, state saved, watch file written. An absent
/// configuration does nothing.
pub fn pass(cfg: &crate::config::Config, now: f64) -> Result<(), String> {
    let Some(a) = &cfg.anomalies else {
        return Ok(());
    };
    let mut st: State = crate::state::load(&cfg.state_dir, "anomalies.json");
    st.observe(a, read(&a.inputs), now);
    crate::state::save(&cfg.state_dir, "anomalies.json", &st)?;
    write_watch(&a.file, &st.watch(now))
}

/// `dismiss CHECK SUBJECT`.
pub fn dismiss(cfg: &crate::config::Config, check: &str, subject: &str) -> Result<String, String> {
    let a = cfg
        .anomalies
        .as_ref()
        .ok_or("no anomalies are configured")?;
    let mut st: State = crate::state::load(&cfg.state_dir, "anomalies.json");
    if !st.dismiss(check, subject) {
        return Err(format!("no active anomaly {}", key(check, subject)));
    }
    crate::state::save(&cfg.state_dir, "anomalies.json", &st)?;
    write_watch(&a.file, &st.watch(crate::now()))?;
    Ok(format!("dismissed {}", key(check, subject)))
}

fn write_watch(path: &Path, file: &File) -> Result<(), String> {
    let tmp = path.with_extension("tmp");
    let text = serde_json::to_vec(file).map_err(|e| e.to_string())?;
    std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(mode: Mode, threshold: f64) -> Anomalies {
        Anomalies {
            inputs: vec![],
            file: "/w.json".into(),
            retention_samples: 20,
            retention_seconds: 10000.0,
            max_links: 3,
            rules: BTreeMap::from([(
                "lat".into(),
                Rule {
                    mode,
                    threshold,
                    min_samples: 3,
                    min_breach_seconds: 100.0,
                    clear_seconds: 100.0,
                },
            )]),
        }
    }
    fn o(at: f64, value: f64) -> Observation {
        Observation {
            check: "lat".into(),
            subject: "svc".into(),
            value,
            at,
            link: Some(format!("https://x/{at}")),
        }
    }
    fn track(st: &State) -> &Track {
        &st.tracks["lat/svc"]
    }

    #[test]
    fn baseline_is_the_median_of_healthy_samples_and_cold_starts_are_unjudged() {
        let r = rules(Mode::Ratio, 3.0);
        let mut st = State::default();
        // Two samples: cold, even for a huge value.
        st.observe(&r, vec![o(10.0, 1.0), o(20.0, 100.0)], 30.0);
        assert!(track(&st).breach_since.is_none() && st.watch(30.0).items.is_empty());
        // Three healthy-looking ones behind it is not enough to judge the
        // 100 as a baseline-setter: it was never judged, so it is in.
        let mut st = State::default();
        st.observe(
            &r,
            vec![o(10.0, 1.0), o(20.0, 3.0), o(30.0, 2.0), o(40.0, 2.0)],
            50.0,
        );
        assert!(!track(&st).history[3].breach);
        st.observe(&r, vec![o(50.0, 6.0)], 60.0);
        // Median of 1,3,2,2 is 2: 6 is 3 times it.
        assert!(track(&st).history[4].breach);
    }

    #[test]
    fn transient_breach_is_not_an_anomaly_but_a_sustained_one_is_stable() {
        let r = rules(Mode::Above, 10.0);
        let mut st = State::default();
        st.observe(&r, (1..=4).map(|i| o(i as f64 * 10.0, 1.0)).collect(), 50.0);
        st.observe(&r, vec![o(60.0, 50.0), o(70.0, 1.0)], 80.0);
        assert!(st.watch(80.0).items.is_empty() && track(&st).breach_since.is_none());
        st.observe(
            &r,
            vec![o(100.0, 50.0), o(150.0, 60.0), o(200.0, 70.0)],
            210.0,
        );
        let w = st.watch(210.0);
        let item = &w.items["lat/svc"];
        assert_eq!(item.since, 100.0);
        assert!(item.what.contains("observed 70") && item.link.as_deref() == Some("https://x/200"));
        // More of it: same stall, same evidence.
        st.observe(&r, vec![o(300.0, 90.0)], 310.0);
        assert_eq!(st.watch(310.0).items["lat/svc"].since, 100.0);
        assert_eq!(st.watch(310.0).items["lat/svc"].what, item.what);
        // Breaching samples stay out of the baseline.
        assert!(track(&st).history.iter().filter(|s| s.breach).count() >= 4);
    }

    #[test]
    fn clears_after_healthy_run_and_flapping_is_one_anomaly() {
        let r = rules(Mode::Above, 10.0);
        let mut st = State::default();
        st.observe(&r, (1..=4).map(|i| o(i as f64 * 10.0, 1.0)).collect(), 50.0);
        st.observe(&r, vec![o(100.0, 50.0), o(200.0, 50.0)], 210.0);
        assert_eq!(st.watch(210.0).items.len(), 1);
        st.observe(
            &r,
            vec![o(220.0, 1.0), o(260.0, 50.0), o(300.0, 1.0)],
            310.0,
        );
        assert_eq!(st.watch(310.0).items["lat/svc"].since, 100.0);
        st.observe(&r, vec![o(350.0, 1.0), o(420.0, 1.0)], 430.0);
        assert!(st.watch(430.0).items.is_empty());
        assert!(track(&st).active.is_none());
    }

    #[test]
    fn duplicates_late_and_missing_data() {
        let r = rules(Mode::Above, 10.0);
        let mut st = State::default();
        st.observe(&r, (1..=4).map(|i| o(i as f64 * 10.0, 1.0)).collect(), 50.0);
        let before = st.clone();
        st.observe(&r, vec![o(40.0, 1.0), o(30.0, 99.0)], 60.0);
        assert_eq!(st, before, "a duplicate time changes nothing");
        // Late (older than judged): history only.
        st.observe(&r, vec![o(35.0, 99.0)], 60.0);
        assert_eq!(track(&st).history.len(), 5);
        assert!(track(&st).breach_since.is_none());
        // Nothing observed, and unreadable inputs: no change.
        st.observe(&r, vec![], 70.0);
        assert!(read(&["/nonexistent/in.json".into()]).is_empty());
        // Unknown checks, NaN-like and future values are dropped.
        let mut bad = o(80.0, f64::INFINITY);
        st.observe(&r, vec![bad.clone()], 90.0);
        bad.value = 1.0;
        bad.at = 9e9;
        st.observe(&r, vec![bad], 90.0);
        assert_eq!(track(&st).history.len(), 5);
    }

    #[test]
    fn missing_data_keeps_an_active_anomaly() {
        let r = rules(Mode::Above, 10.0);
        let mut st = State::default();
        st.observe(&r, (1..=4).map(|i| o(i as f64 * 10.0, 1.0)).collect(), 50.0);
        st.observe(&r, vec![o(100.0, 50.0), o(200.0, 50.0)], 210.0);
        st.observe(&r, vec![], 5000.0);
        assert_eq!(st.watch(5000.0).items.len(), 1);
    }

    #[test]
    fn retention_bounds_samples_and_age() {
        let r = rules(Mode::Above, 1e9);
        let mut st = State::default();
        st.observe(
            &r,
            (1..=40).map(|i| o(i as f64 * 10.0, 1.0)).collect(),
            400.0,
        );
        assert_eq!(track(&st).history.len(), 20);
        assert_eq!(track(&st).history[0].at, 210.0);
        st.observe(&r, vec![], 10300.0);
        assert!(track(&st).history.iter().all(|s| s.at >= 300.0));
        st.observe(&r, vec![], 20000.0);
        assert!(st.tracks.is_empty());
    }

    #[test]
    fn dismissal_holds_the_anomaly_out_until_it_clears() {
        let r = rules(Mode::Above, 10.0);
        let mut st = State::default();
        assert!(!st.dismiss("lat", "svc"));
        st.observe(&r, (1..=4).map(|i| o(i as f64 * 10.0, 1.0)).collect(), 50.0);
        st.observe(&r, vec![o(100.0, 50.0), o(200.0, 50.0)], 210.0);
        assert!(st.dismiss("lat", "svc"));
        assert!(st.watch(220.0).items.is_empty());
        st.observe(&r, vec![o(300.0, 50.0)], 310.0);
        assert!(st.watch(310.0).items.is_empty());
        st.observe(&r, vec![o(400.0, 1.0), o(500.0, 1.0)], 510.0);
        assert!(!track(&st).dismissed);
        // A new breach after clearing is a new anomaly.
        st.observe(&r, vec![o(600.0, 50.0), o(700.0, 50.0)], 710.0);
        assert_eq!(st.watch(710.0).items["lat/svc"].since, 600.0);
    }

    #[test]
    fn below_mode_and_restart_persistence() {
        let r = rules(Mode::Below, 0.5);
        let dir = tempfile::tempdir().unwrap();
        let mut st = State::default();
        st.observe(&r, (1..=4).map(|i| o(i as f64 * 10.0, 5.0)).collect(), 50.0);
        st.observe(&r, vec![o(100.0, 0.0), o(200.0, 0.0)], 210.0);
        crate::state::save(dir.path(), "anomalies.json", &st).unwrap();
        let mut again: State = crate::state::load(dir.path(), "anomalies.json");
        assert_eq!(again, st);
        // The same observations again after a restart: nothing new.
        again.observe(&r, vec![o(100.0, 0.0), o(200.0, 0.0)], 220.0);
        assert_eq!(again.watch(220.0).items, st.watch(220.0).items);
    }
}
