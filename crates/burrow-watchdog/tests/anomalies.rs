//! Anomalies and degradations take one notice lifecycle: the ladder's.
mod common;

use std::collections::BTreeMap;
use std::fs;

use burrow_watchdog::anomaly::dismiss;
use burrow_watchdog::config::{Anomalies, Mode, Rule, Source};
use burrow_watchdog::{GRACE, MIN};
use common::{H, Machine, T, named};
use serde_json::json;

fn machine() -> (Machine, std::path::PathBuf, std::path::PathBuf) {
    let mut m = Machine::new();
    let dir = m.cfg.state_dir.clone();
    let (input, watch, degraded) = (
        dir.join("obs.json"),
        dir.join("anomalies-watch.json"),
        dir.join("degradations.json"),
    );
    for (name, file) in [("anomalies", &watch), ("degradations", &degraded)] {
        m.cfg.sources.insert(
            name.into(),
            Source {
                file: file.clone(),
                about: name.into(),
                look: "look.".into(),
                kick: None,
                stale: None,
            },
        );
    }
    m.cfg.anomalies = Some(Anomalies {
        inputs: vec![input.clone()],
        file: watch,
        retention_samples: 50,
        retention_seconds: 7.0 * 86400.0,
        max_links: 3,
        rules: BTreeMap::from([(
            "latency".into(),
            Rule {
                mode: Mode::Ratio,
                threshold: 3.0,
                min_samples: 3,
                min_breach_seconds: 10.0 * MIN,
                clear_seconds: 10.0 * MIN,
            },
        )]),
    });
    (m, input, degraded)
}

fn observe(input: &std::path::Path, samples: &[(f64, f64)]) {
    let obs: Vec<_> = samples
        .iter()
        .map(|(at, v)| {
            json!({"check": "latency", "subject": "svc", "value": v, "at": at,
            "link": format!("https://example.org/{at}")})
        })
        .collect();
    fs::write(input, json!({"observations": obs}).to_string()).unwrap();
}

#[test]
fn degradation_and_anomaly_share_one_notice_lifecycle() {
    let (m, input, degraded) = machine();
    m.write(&m.steward, json!({}), T);
    let healthy: Vec<_> = (0..4).map(|i| (T - H + i as f64 * MIN, 1.0)).collect();
    observe(&input, &healthy);
    m.run(T);
    assert!(m.runs().is_empty());
    // Sustained breach, with a degradation signal alongside it.
    let mut all = healthy.clone();
    all.extend([(T, 9.0), (T + 5.0 * MIN, 9.0), (T + 12.0 * MIN, 9.0)]);
    observe(&input, &all);
    m.write(
        &degraded,
        json!({"sam/r/ci/cache": named("sam/r/ci: degraded/cache", "repeated workflow degradation", T, T)}),
        T,
    );
    let (_, stalls) = m.run(T + 13.0 * MIN);
    assert_eq!(stalls.len(), 2);
    let ids: Vec<_> = stalls.keys().cloned().collect();
    assert!(
        ids.iter()
            .any(|s| s.starts_with("anomalies\tlatency/svc\t"))
    );
    // One job gets both; nothing repeats over many passes and restarts.
    let (wake, _) = m.run(T + 13.0 * MIN + GRACE);
    let _ = wake;
    for i in 1..6 {
        m.run(T + 14.0 * MIN + GRACE + i as f64 * MIN);
    }
    assert_eq!(m.runs().len(), 1, "{:?}", m.runs());
    assert!(m.runs()[0][12].contains("anomaly latency/svc"));
    assert!(m.runs()[0][12].contains("degraded/cache"));
}

#[test]
fn persistent_evidence_after_dismissal_launches_nothing_more() {
    let (m, input, _) = machine();
    m.write(&m.steward, json!({}), T);
    let mut all: Vec<_> = (0..4).map(|i| (T - H + i as f64 * MIN, 1.0)).collect();
    all.extend([(T, 9.0), (T + 12.0 * MIN, 9.0)]);
    observe(&input, &all);
    let now = T + 13.0 * MIN;
    let (_, stalls) = m.run(now);
    assert_eq!(stalls.len(), 1);
    let before = m.state().stalls.len();
    assert_eq!(before, 1);
    let msg = dismiss(&m.cfg, "latency", "svc").unwrap();
    assert!(msg.contains("latency/svc"));
    // The stall is gone and the evidence going on opens none again, over
    // repeated passes and more breaching samples.
    for i in 1..5 {
        all.push((now + i as f64 * 10.0 * MIN, 9.0));
        observe(&input, &all);
        let (_, stalls) = m.run(now + i as f64 * 10.0 * MIN + 1.0);
        assert!(stalls.is_empty());
    }
    assert_eq!(m.runs().len(), 1, "only the job from before the dismissal");
    assert!(dismiss(&m.cfg, "latency", "other").is_err());
}
