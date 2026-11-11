//! The snapshot's schema, and what its JSON is held to. The stall it was
//! built for is in [`crate::inbox`]'s view tests, which have the inbox's
//! stand-ins.

use std::fs;

use serde_json::Value;

use super::*;
use crate::config::tests::example;

/// The checked-in schema's path, from the crate's root.
const SCHEMA: &str = "state.schema.json";

/// Whether JSON follows the checked-in schema; why not when it doesn't.
pub(crate) fn validates(json: &Value) -> Result<(), String> {
    let schema: Value = serde_json::from_str(include_str!("../../state.schema.json")).unwrap();
    let mut schemas = boon::Schemas::new();
    let mut compiler = boon::Compiler::new();
    compiler
        .add_resource("urn:proj-state", schema)
        .map_err(|e| e.to_string())?;
    let index = compiler
        .compile("urn:proj-state", &mut schemas)
        .map_err(|e| e.to_string())?;
    schemas.validate(json, index).map_err(|e| format!("{e:#}"))
}

// The checked-in schema is the one the types make: a change to the shape
// changes the file in the same commit. UPDATE_SCHEMA=1 writes it afresh.
#[test]
fn the_schema_is_checked_in() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SCHEMA);
    let made = Snapshot::schema();
    if std::env::var_os("UPDATE_SCHEMA").is_some() {
        fs::write(&path, &made).unwrap();
    }
    assert!(
        include_str!("../../state.schema.json") == made,
        "{SCHEMA} isn't the snapshot's schema: run UPDATE_SCHEMA=1 cargo test -p burrow-proj the_schema_is_checked_in"
    );
}

/// A snapshot with every field filled and every kind of slot.
fn full() -> Snapshot {
    let holder = Holder {
        job: Some("j1".into()),
        project: Some("/src/box".into()),
        issue: Some("https://github.com/sam/box/issues/7".into()),
        role: Some("inbox".into()),
        class: Some("lead".into()),
        engine: Some(Engine::Codex),
        model: Some("small".into()),
        effort: Some("high".into()),
        awaits: vec!["sam/box#8".into()],
    };
    let kinds = [
        SlotKind::Claude,
        SlotKind::Unstick,
        SlotKind::Codex,
        SlotKind::Drain,
        SlotKind::Sorting,
        SlotKind::Heavy,
    ];
    let states = [
        SlotState::Idle,
        SlotState::Running,
        SlotState::Queued,
        SlotState::Parked,
        SlotState::Asking,
        SlotState::Ended,
        SlotState::Starting,
        SlotState::Busy,
    ];
    let window = Window {
        used: 42.5,
        resets_at: 1_800_000_000,
    };
    Snapshot {
        version: VERSION,
        at: 1_800_000_000,
        queue: vec![Ready {
            repo: "sam/box".into(),
            number: 7,
            priority: Priority::High,
            pipeline: true,
            repair: false,
            opened: "2026-11-01T00:00:00Z".into(),
            age: Some(60),
        }],
        line: vec![Waiting {
            job: "j2".into(),
            class: "first".into(),
            project: "/src/box".into(),
            since: 1_799_999_000,
            wait: 1000,
        }],
        slots: kinds
            .iter()
            .flat_map(|k| {
                states.iter().map(|s| Slot {
                    holder: Some(holder.clone()),
                    ..held(*k, "1".into(), *s, Some(1_799_999_000), 1_800_000_000)
                })
            })
            .collect(),
        meters: vec![Meter {
            engine: Engine::Claude,
            reading: Some(Reading {
                five_hour: Some(window.clone()),
                weekly: Some(window),
                credits: true,
                read_at: 1_799_999_000,
                age: 1000,
                fresh: true,
            }),
            hold: Some(UsageHold {
                owner: true,
                since: 1_799_999_000,
                until: 1_800_003_600,
                reason: "paused".into(),
            }),
        }],
        holds: Holds {
            paused: vec!["sam/game".into()],
            pipeline: Pipeline {
                repos: vec!["sam/burrow".into()],
                lead: vec!["sam/burrow".into()],
                slots: 1,
            },
            triage: Some(Triage {
                drain: Some(Engine::Codex),
                drain_jobs: 3,
                sorting_at_once: 2,
            }),
        },
        jobs: vec![JobRow {
            id: "j1".into(),
            state: "done".into(),
            vm: vec!["vm-1".into()],
            apps: vec!["workers".into()],
            demoted: Some("not pipeline work".into()),
            waiting_in_line: vec!["heavy".into()],
            awaits: vec!["sam/box#8".into()],
            ends: vec!["ask".into()],
            answer: "all done".into(),
        }],
    }
}

// Every field and every kind of slot and state follows the schema, and one
// that doesn't is caught: the check has teeth.
#[test]
fn the_json_follows_the_schema() {
    let full = serde_json::to_value(full()).unwrap();
    validates(&full).unwrap();
    // It reads back as it was.
    assert_eq!(
        serde_json::from_value::<Snapshot>(full.clone()).unwrap(),
        self::full()
    );
    for broken in [
        ("/slots/0/state", Value::from("napping")),
        ("/version", Value::from("one")),
        ("/meters/0/reading/fiveHour/used", Value::Null),
    ] {
        let mut bad = full.clone();
        *bad.pointer_mut(broken.0).unwrap() = broken.1;
        assert!(validates(&bad).is_err(), "{}", broken.0);
    }
    let mut extra = full;
    extra["prompt"] = Value::from("text");
    assert!(validates(&extra).is_err(), "an unknown field");
}

// A machine with nothing going: every slot idle, its meters as they were
// read, and `proj state --json` follows the schema.
#[test]
fn an_idle_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.jobs = tmp.path().join("jobs");
    cfg.projects = tmp.path().join("src");
    cfg.inbox = None;
    let meters = tmp.path().join("meters");
    fs::create_dir_all(&meters).unwrap();
    fs::write(
        meters.join("codex.json"),
        r#"{"credits":true,"read_at":1000.0,"windows":{"10080m":{"resets_at":5000.0,"used":100.0},"300m":{"resets_at":2000.0,"used":3.0}}}"#,
    )
    .unwrap();
    cfg.meters = Some(meters);
    let heavy = tmp.path().join("heavy");
    fs::create_dir_all(&heavy).unwrap();
    for i in 1..=2 {
        fs::write(burrow_heavy::config::slot_file(&heavy, i), "").unwrap();
    }
    cfg.heavy_dir = Some(heavy.clone());
    // One heavy slot is busy, held by nothing of proj's.
    let busy = fs::File::open(burrow_heavy::config::slot_file(&heavy, 2)).unwrap();
    busy.lock().unwrap();
    let json: Value = serde_json::from_str(&crate::cmd::state_json(&cfg, 15).unwrap()).unwrap();
    validates(&json).unwrap();
    let snap: Snapshot = serde_json::from_value(json).unwrap();
    assert_eq!(snap.version, VERSION);
    let kinds: Vec<_> = snap.slots.iter().map(|s| (s.kind, s.state)).collect();
    use SlotKind::*;
    use SlotState::*;
    assert_eq!(
        kinds,
        [
            (Claude, Idle),
            (Claude, Idle),
            (Claude, Idle),
            (Claude, Idle),
            (Unstick, Idle),
            (Codex, Idle),
            (Codex, Idle),
            (Heavy, Idle),
            (Heavy, Busy),
        ]
    );
    assert_eq!(snap.meters[0].reading, None, "no Claude reading");
    let codex = snap.meters[1].reading.as_ref().unwrap();
    assert_eq!(
        (
            codex.five_hour.as_ref().unwrap().used,
            codex.weekly.as_ref().unwrap().used
        ),
        (3.0, 100.0)
    );
    assert!(
        codex.credits && !codex.fresh,
        "an old reading is shown, as old"
    );
    assert_eq!(codex.age, snap.at - 1000);
    assert!(snap.queue.is_empty() && snap.line.is_empty() && snap.jobs.is_empty());
    assert_eq!(snap.status(), "");
}
