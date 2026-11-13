use super::*;
use burrow_usage_reset::meter::FRESH;
use serde_json::json;

const NOW: i64 = 100_000;

/// Puts a meter file in place, or none.
type Writes<'a> = &'a dyn Fn(&Path);

fn pref(engine: &str, model: &str) -> Preference {
    Preference {
        engine: engine.into(),
        model: Some(model.into()),
        effort: Some("high".into()),
    }
}

fn prefs(engines: &[&str]) -> Vec<Preference> {
    engines.iter().map(|e| pref(e, &format!("{e}-m"))).collect()
}

/// ENGINE's meter in DIR as burrow-usage-reset writes it, read AGE seconds ago.
fn meter(dir: &Path, engine: &str, windows: &[(&str, f64)], credits: bool, age: i64) {
    let windows: serde_json::Map<_, _> = windows
        .iter()
        .map(|(n, u)| ((*n).to_owned(), json!({"used": u, "resets_at": NOW + 3600})))
        .collect();
    std::fs::write(
        dir.join(format!("{engine}.json")),
        json!({"windows": windows, "credits": credits, "read_at": NOW - age}).to_string(),
    )
    .unwrap();
}

fn policy(claude: f64, codex: f64, credits: bool) -> Policy {
    Policy::new(
        BTreeMap::from([("claude".into(), claude), ("codex".into(), codex)]),
        credits,
    )
    .unwrap()
}

fn inputs(dir: &Path, holds: &[&str]) -> Inputs {
    let holds = holds
        .iter()
        .map(|e| {
            (
                (*e).to_owned(),
                Hold {
                    until: NOW + 600,
                    reason: "limit reached".into(),
                    owner: false,
                },
            )
        })
        .collect();
    Inputs::read(Installed::builtin(), holds, Some(dir), NOW)
}

fn picked(d: &Decision) -> Option<(usize, &str, Funding)> {
    match &d.outcome {
        Outcome::Selected {
            index,
            choice,
            funding,
        } => Some((*index, choice.engine.as_str(), *funding)),
        Outcome::Blocked => None,
    }
}

fn reasons(d: &Decision) -> Vec<(usize, &Reason)> {
    d.skipped.iter().map(|s| (s.index, &s.reason)).collect()
}

/// 11-11: Codex's week at 100% on credits while Claude's plan had room. The
/// old overflow line kept the turn on Codex's credits (credits made it never
/// "short"); the selector takes Claude's plan, and says why Codex was passed.
#[test]
fn codex_weekly_exhausted_on_credits_yields_to_claudes_plan() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    meter(dir, "codex", &[("300m", 10.0), ("10080m", 100.0)], true, 60);
    meter(
        dir,
        "claude",
        &[("five_hour", 20.0), ("seven_day", 40.0)],
        false,
        60,
    );

    // The old path, with the same meters: the regression.
    let (_, old) = crate::engine::role("work codex over 90 claude", Some(dir), false, NOW).unwrap();
    assert_eq!(old, Engine::Codex, "old overflow stays on Codex's credits");

    let d = select(
        &prefs(&["codex", "claude"]),
        &policy(90.0, 90.0, true),
        &inputs(dir, &[]),
    );
    assert_eq!(picked(&d), Some((1, "claude", Funding::Plan)));
    assert_eq!(
        reasons(&d),
        [(
            0,
            &Reason::Threshold {
                window: "10080m".into(),
                used: 100.0,
                threshold: 90.0,
                credits: true
            }
        )]
    );
    assert!(d.degraded());
    // Its evidence is both engines' fresh readings.
    assert!(
        d.meters
            .values()
            .all(|e| matches!(e, Evidence::Fresh { .. }))
    );
    assert_eq!(d.meters.len(), 2);
    // The model and effort travel with the choice.
    let Outcome::Selected { choice, .. } = &d.outcome else {
        unreachable!()
    };
    assert_eq!(choice, &pref("claude", "claude-m"));
}

#[test]
fn both_plans_out_spend_credits_only_when_the_role_allows() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    meter(dir, "claude", &[("seven_day", 100.0)], false, 60);
    meter(dir, "codex", &[("300m", 5.0), ("10080m", 100.0)], true, 60);
    let list = prefs(&["claude", "codex"]);

    let allowed = select(&list, &policy(95.0, 95.0, true), &inputs(dir, &[]));
    assert_eq!(picked(&allowed), Some((1, "codex", Funding::Credits)));
    assert_eq!(reasons(&allowed).len(), 1);
    assert_eq!(reasons(&allowed)[0].0, 0);
    assert!(allowed.credits_permitted && allowed.degraded());

    let forbidden = select(&list, &policy(95.0, 95.0, false), &inputs(dir, &[]));
    assert_eq!(picked(&forbidden), None);
    assert!(!forbidden.credits_permitted && forbidden.degraded());
    assert_eq!(
        reasons(&forbidden),
        [
            (
                0,
                &Reason::Threshold {
                    window: "seven_day".into(),
                    used: 100.0,
                    threshold: 95.0,
                    credits: false
                }
            ),
            (
                1,
                &Reason::Threshold {
                    window: "10080m".into(),
                    used: 100.0,
                    threshold: 95.0,
                    credits: true
                }
            ),
        ]
    );

    // Credits go to the first choice in the list that has them.
    meter(dir, "claude", &[("seven_day", 100.0)], true, 60);
    let first = select(&list, &policy(95.0, 95.0, true), &inputs(dir, &[]));
    assert_eq!(picked(&first), Some((0, "claude", Funding::Credits)));
    assert!(first.skipped.iter().all(|s| s.index == 1));
}

#[test]
fn a_window_at_its_threshold_has_no_headroom_and_one_under_has() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    meter(dir, "codex", &[("300m", 1.0)], false, 60);
    let list = prefs(&["claude", "codex"]);
    for (used, threshold, expected) in [
        (79.999, 80.0, (0, "claude")),
        (80.0, 80.0, (1, "codex")),
        (80.001, 80.0, (1, "codex")),
        (99.999, 100.0, (0, "claude")),
        (100.0, 100.0, (1, "codex")),
        (0.0, 0.0, (1, "codex")),
    ] {
        // The five-hour window counts as much as the week.
        meter(
            dir,
            "claude",
            &[("five_hour", used), ("seven_day", 1.0)],
            false,
            60,
        );
        let d = select(&list, &policy(threshold, 90.0, false), &inputs(dir, &[]));
        assert_eq!(
            picked(&d),
            Some((expected.0, expected.1, Funding::Plan)),
            "{used} against {threshold}"
        );
    }
    // Each engine is held to its own threshold, and one with none to 100.
    meter(dir, "claude", &[("seven_day", 85.0)], false, 60);
    meter(dir, "codex", &[("10080m", 85.0)], false, 60);
    let d = select(&list, &policy(80.0, 90.0, false), &inputs(dir, &[]));
    assert_eq!(picked(&d), Some((1, "codex", Funding::Plan)));
    let unset = Policy::new(BTreeMap::new(), false).unwrap();
    assert_eq!(unset.threshold("claude"), 100.0);
    let d = select(&list, &unset, &inputs(dir, &[]));
    assert_eq!(picked(&d), Some((0, "claude", Funding::Plan)));
    // A threshold outside 0 to 100 is refused.
    for bad in [-0.1, 100.1, f64::NAN] {
        assert!(Policy::new(BTreeMap::from([("codex".into(), bad)]), true).is_err());
    }
}

#[test]
fn a_hold_is_never_run_and_never_moves_onto_an_unread_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    meter(dir, "claude", &[("seven_day", 1.0)], false, 60);
    meter(dir, "codex", &[("10080m", 1.0)], true, 60);
    let list = prefs(&["claude", "codex"]);
    let p = policy(90.0, 90.0, true);

    // Held whatever its meter says: the next choice with room runs.
    let d = select(&list, &p, &inputs(dir, &["claude"]));
    assert_eq!(picked(&d), Some((1, "codex", Funding::Plan)));
    assert!(matches!(reasons(&d)[..], [(0, Reason::Held { .. })]));

    // Every engine held: blocked, credits or not.
    let d = select(&list, &p, &inputs(dir, &["claude", "codex"]));
    assert_eq!(picked(&d), None);
    assert_eq!(d.skipped.len(), 2);

    // A hold is a usage move: nothing moves onto an engine with no reading.
    std::fs::remove_file(dir.join("codex.json")).unwrap();
    let d = select(&list, &p, &inputs(dir, &["claude"]));
    assert_eq!(picked(&d), None);
    assert!(matches!(
        reasons(&d)[..],
        [(0, Reason::Held { .. }), (1, Reason::Unmetered)]
    ));
}

#[test]
fn an_unfit_reading_moves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let fresh_room = |dir: &Path| meter(dir, "codex", &[("10080m", 1.0)], false, 60);
    let list = prefs(&["claude", "codex"]);
    let p = policy(80.0, 80.0, true);
    let cases: [(&str, Writes); 6] = [
        ("missing", &|_| {}),
        ("malformed", &|d| {
            std::fs::write(d.join("claude.json"), "broken").unwrap()
        }),
        ("another program's", &|d| {
            std::fs::write(d.join("claude.json"), r#"{"utilization": 99}"#).unwrap()
        }),
        ("out of range", &|d| {
            meter(d, "claude", &[("seven_day", 101.0)], false, 60)
        }),
        ("future", &|d| {
            meter(d, "claude", &[("seven_day", 100.0)], false, -1)
        }),
        ("stale", &|d| {
            meter(d, "claude", &[("seven_day", 100.0)], false, FRESH as i64)
        }),
    ];
    for (what, write) in cases {
        let _ = std::fs::remove_file(dir.join("claude.json"));
        fresh_room(dir);
        write(dir);
        // First in the list: it runs as it stands, though Codex has room.
        let d = select(&list, &p, &inputs(dir, &[]));
        assert_eq!(
            picked(&d),
            Some((0, "claude", Funding::Unmetered)),
            "{what}"
        );
        assert!(d.skipped.is_empty() && !d.degraded(), "{what}");
        assert!(
            matches!(d.meters["claude"], Evidence::Unfit { .. }),
            "{what}: {:?}",
            d.meters["claude"]
        );
        // After a usage move: never moved onto.
        meter(dir, "codex", &[("10080m", 100.0)], true, 60);
        let d = select(&prefs(&["codex", "claude"]), &p, &inputs(dir, &[]));
        assert_eq!(picked(&d), Some((0, "codex", Funding::Credits)), "{what}");
        assert!(
            matches!(reasons(&d)[..], [(1, Reason::Unmetered)]),
            "{what}"
        );
        let d = select(
            &prefs(&["codex", "claude"]),
            &policy(80.0, 80.0, false),
            &inputs(dir, &[]),
        );
        assert_eq!(picked(&d), None, "{what}");
    }
    // Each says why.
    let why = |write: &dyn Fn(&Path)| {
        let _ = std::fs::remove_file(dir.join("claude.json"));
        write(dir);
        match Evidence::read(Some(dir), "claude", NOW) {
            Evidence::Unfit { why } => why,
            Evidence::Fresh { .. } => panic!("fresh"),
        }
    };
    assert_eq!(why(&|_| {}), Unfit::Missing);
    assert!(matches!(
        why(&|d| std::fs::write(d.join("claude.json"), "x").unwrap()),
        Unfit::Malformed { .. }
    ));
    assert_eq!(
        why(&|d| meter(d, "claude", &[("seven_day", 101.0)], false, 60)),
        Unfit::OutOfRange {
            window: "seven_day".into(),
            used: 101.0
        }
    );
    assert_eq!(
        why(&|d| meter(d, "claude", &[], false, -1)),
        Unfit::Future {
            read_at: (NOW + 1) as f64
        }
    );
    assert_eq!(
        why(&|d| meter(d, "claude", &[], false, FRESH as i64)),
        Unfit::Stale {
            read_at: NOW as f64 - FRESH
        }
    );
    // A fresh reading just inside the hour still counts.
    meter(dir, "claude", &[], false, FRESH as i64 - 1);
    assert!(matches!(
        Evidence::read(Some(dir), "claude", NOW),
        Evidence::Fresh { .. }
    ));
    // No meters dir at all is no reading.
    assert_eq!(
        Evidence::read(None, "claude", NOW),
        Evidence::Unfit {
            why: Unfit::Missing
        }
    );
}

#[test]
fn only_installed_engines_run() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    meter(dir, "claude", &[("seven_day", 1.0)], false, 60);
    meter(dir, "codex", &[("10080m", 1.0)], false, 60);
    meter(dir, "local", &[("week", 1.0)], false, 60);
    let p = policy(90.0, 90.0, true);
    let list = prefs(&["local", "codex", "claude"]);

    // No declarations: the two built-ins, nothing else.
    assert_eq!(
        Installed::declared(None::<Vec<String>>),
        Installed::builtin()
    );
    let d = select(&list, &p, &inputs(dir, &[]));
    assert_eq!(picked(&d), Some((1, "codex", Funding::Plan)));
    assert_eq!(reasons(&d), [(0, &Reason::Unavailable)]);
    // Not installed isn't a usage move: an unread engine after it still runs.
    std::fs::remove_file(dir.join("codex.json")).unwrap();
    let d = select(&list, &p, &inputs(dir, &[]));
    assert_eq!(picked(&d), Some((1, "codex", Funding::Unmetered)));

    // Declarations name what's installed, built-ins included or not.
    let declared = Installed::declared(Some(["local", "claude"]));
    let read = Inputs::read(declared, BTreeMap::new(), Some(dir), NOW);
    assert!(!read.meters.contains_key("codex"));
    let d = select(&list, &p, &read);
    assert_eq!(picked(&d), Some((0, "local", Funding::Plan)));
    let d = select(&prefs(&["codex", "claude"]), &p, &read);
    assert_eq!(picked(&d), Some((1, "claude", Funding::Plan)));
    assert_eq!(reasons(&d), [(0, &Reason::Unavailable)]);

    // Nothing installed that the list names: blocked.
    let none = Inputs::read(
        Installed::declared(Some(["local"])),
        BTreeMap::new(),
        Some(dir),
        NOW,
    );
    let d = select(&prefs(&["codex", "claude"]), &p, &none);
    assert_eq!(picked(&d), None);
}

#[test]
fn the_same_inputs_give_the_same_decision_and_the_list_order_rules() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    meter(
        dir,
        "claude",
        &[("five_hour", 95.0), ("seven_day", 95.0)],
        true,
        60,
    );
    meter(dir, "codex", &[("300m", 10.0)], false, 60);
    let list = vec![
        pref("claude", "big"),
        pref("claude", "small"),
        pref("codex", "big"),
        pref("codex", "small"),
    ];
    let p = policy(90.0, 90.0, true);
    let first = select(&list, &p, &inputs(dir, &[]));
    for _ in 0..5 {
        assert_eq!(select(&list, &p, &inputs(dir, &[])), first);
    }
    // Plan anywhere beats the earlier choices' credits; the first plan wins.
    assert_eq!(picked(&first), Some((2, "codex", Funding::Plan)));
    let Outcome::Selected { choice, .. } = &first.outcome else {
        unreachable!()
    };
    assert_eq!(choice.model.as_deref(), Some("big"));
    // Both Claude choices passed over, each naming the same window: the
    // furthest past, the first by name among equals.
    let spent = Reason::Threshold {
        window: "five_hour".into(),
        used: 95.0,
        threshold: 90.0,
        credits: true,
    };
    assert_eq!(reasons(&first), [(0, &spent), (1, &spent)]);
    // Reversed, the other engine leads.
    let reversed: Vec<_> = list.iter().rev().cloned().collect();
    let d = select(&reversed, &p, &inputs(dir, &[]));
    assert_eq!(picked(&d), Some((0, "codex", Funding::Plan)));
    assert!(d.skipped.is_empty() && !d.degraded());

    // It persists and reads back whole.
    let text = serde_json::to_string(&first).unwrap();
    assert_eq!(serde_json::from_str::<Decision>(&text).unwrap(), first);
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["outcome"], "selected");
    assert_eq!(value["funding"], "plan");
    assert_eq!(value["skipped"][0]["reason"], "threshold");
    assert_eq!(value["meters"]["claude"]["state"], "fresh");
    let blocked = select(&list, &p, &inputs(dir, &["claude", "codex"]));
    let text = serde_json::to_string(&blocked).unwrap();
    assert_eq!(serde_json::from_str::<Decision>(&text).unwrap(), blocked);

    // An empty list selects nothing.
    assert_eq!(picked(&select(&[], &p, &inputs(dir, &[]))), None);
}
