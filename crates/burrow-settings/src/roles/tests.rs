use super::*;
use crate::{Config, Migration};
use std::fs;

fn rig() -> (tempfile::TempDir, Config) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        overlay: dir.path().join("overlay.json"),
        ..Default::default()
    };
    (dir, cfg)
}

fn set(cfg: &Config, line: &str) -> Result<String, String> {
    let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
    cfg.command(&args, "test")
}

fn overlay(cfg: &Config) -> serde_json::Value {
    serde_json::from_slice(&fs::read(&cfg.overlay).unwrap()).unwrap()
}

#[test]
fn a_role_with_nothing_set_runs_on_the_declared_defaults() {
    let (_d, cfg) = rig();
    let force = cfg.roles().unwrap();
    assert_eq!(force.route("triage", None).engine, "claude");
    assert_eq!(force.route("planner", Some("sam/box")).engine, "codex");
    // Its model is the declared `* ENGINE` entry, shown as one: null, the
    // engine's own.
    assert_eq!(force.models["* claude"], Tuning::default());
    assert_eq!(force.tuning("triage", None, "claude"), Tuning::default());
    assert!(force.offers["claude"].efforts.contains(&"max".to_owned()));
}

#[test]
fn the_closest_key_wins() {
    let (_d, cfg) = rig();
    set(&cfg, "role * codex").unwrap();
    set(&cfg, "role fix@Sam/Box claude over 80 codex").unwrap();
    set(&cfg, "role *@sam/web claude").unwrap();
    let force = cfg.roles().unwrap();
    assert_eq!(force.route("other", None).engine, "codex");
    assert_eq!(force.route("other", Some("sam/web")).engine, "claude");
    assert_eq!(force.route("fix", Some("sam/web")).engine, "claude");
    let fix = force.route("fix", Some("SAM/box"));
    assert_eq!(
        (fix.engine.as_str(), fix.over.as_ref().map(|o| o.pct)),
        ("claude", Some(80))
    );
    assert_eq!(force.route("fix", None).engine, "codex");
    // The repo is stored folded, so its next setting replaces it.
    set(&cfg, "role fix@sam/box codex").unwrap();
    assert_eq!(
        cfg.roles().unwrap().route("fix", Some("sam/box")).engine,
        "codex"
    );
    set(&cfg, "role fix@SAM/BOX unset").unwrap();
    assert_eq!(
        cfg.roles().unwrap().route("fix", Some("sam/box")).engine,
        "codex"
    );
    assert!(!cfg.roles().unwrap().roles.contains_key("fix@sam/box"));
    for bad in [
        "role fix gemini",
        "role fix codex over 101 claude",
        "role fix codex over 80",
        "role fix@owner codex",
        "role fix",
    ] {
        assert!(set(&cfg, bad).is_err(), "{bad}");
    }
}

#[test]
fn an_unknown_model_or_effort_is_refused() {
    let (_d, cfg) = rig();
    set(&cfg, "pause sam/box").unwrap();
    let before = fs::read(&cfg.overlay).unwrap();
    let err = set(&cfg, "model fix claude big-one").unwrap_err();
    assert!(err.contains("claude doesn't offer that model"), "{err}");
    let err = set(&cfg, "model fix claude - enormous").unwrap_err();
    assert!(err.contains("doesn't offer effort enormous"), "{err}");
    // Codex's efforts are its own: max is Claude's.
    assert!(set(&cfg, "model fix codex - max").is_err());
    assert_eq!(fs::read(&cfg.overlay).unwrap(), before, "nothing changed");

    set(&cfg, "offer claude models big-one small").unwrap();
    set(&cfg, "model fix claude big-one high").unwrap();
    set(&cfg, "model fix codex - medium").unwrap();
    let force = cfg.roles().unwrap();
    assert_eq!(
        force.tuning("fix", Some("sam/box"), "claude"),
        Tuning {
            model: Some("big-one".into()),
            effort: Some("high".into())
        }
    );
    assert_eq!(
        force.tuning("fix", None, "codex").effort.as_deref(),
        Some("medium")
    );
    // An offer a role is set to can't be withdrawn under it.
    let err = set(&cfg, "offer claude models small").unwrap_err();
    assert!(err.contains("doesn't offer that model"), "{err}");
    set(&cfg, "model fix claude unset").unwrap();
    set(&cfg, "offer claude models small").unwrap();
    // A hand edit that names an unoffered model fails every read.
    let mut edited = overlay(&cfg);
    edited["routing"]["models"]["judge claude"] =
        serde_json::json!({"model": "other", "effort": null});
    fs::write(&cfg.overlay, edited.to_string()).unwrap();
    assert!(cfg.roles().is_err());
}

#[test]
fn every_change_is_logged() {
    let (_d, cfg) = rig();
    set(&cfg, "offer codex models mid").unwrap();
    set(&cfg, "model triage codex mid low").unwrap();
    assert!(set(&cfg, "model triage codex other").is_err());
    let log = fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "a refused change isn't one: {log}");
    assert_eq!(
        lines[1]["change"],
        serde_json::json!(["model", "triage", "codex", "mid", "low"])
    );
    assert_eq!(lines[1]["actor"], "test");
}

#[test]
fn engines_use_changes_exactly_the_saved_values() {
    let (_d, cfg) = rig();
    set(&cfg, "offer claude models small big").unwrap();
    set(&cfg, "role triage codex").unwrap();
    set(&cfg, "model triage claude small low").unwrap();
    set(&cfg, "engines save cheap").unwrap();
    assert!(
        set(&cfg, "engines save cheap").is_err(),
        "save needs --replace"
    );
    set(&cfg, "role triage claude").unwrap();
    set(&cfg, "role fix codex over 80 claude").unwrap();
    set(&cfg, "model triage claude big").unwrap();
    let said = set(&cfg, "engines save usual").unwrap();
    assert!(said.contains("Active: usual"), "{said}");
    // What isn't a role setting: a pause, a scope, the offers.
    set(&cfg, "pause sam/box").unwrap();
    set(&cfg, &format!("pipeline {}", crate::now() + 600)).unwrap();
    let before = overlay(&cfg);

    let said = set(&cfg, "engines use cheap").unwrap();
    assert!(
        said.contains("Was active: usual\nActive: cheap\nPrevious: usual\n"),
        "{said}"
    );
    // No model id in what the door answers.
    assert!(!said.contains("small") && !said.contains("big"), "{said}");
    let after = overlay(&cfg);
    let saved = &before["setups"]["saved"]["cheap"];
    assert_eq!(after["routing"], *saved);
    for (key, value) in before.as_object().unwrap() {
        if key != "routing" && key != "setups" {
            assert_eq!(after[key], *value, "{key} changed");
        }
    }
    assert_eq!(after["setups"]["saved"], before["setups"]["saved"]);
    let force = cfg.roles().unwrap();
    assert_eq!(force.route("triage", None).engine, "codex");
    assert_eq!(
        force.route("fix", None).engine,
        "claude",
        "fix is default again"
    );
    assert_eq!(
        force.tuning("triage", None, "claude").model.as_deref(),
        Some("small")
    );

    // Back is what was in force, and back again is this.
    set(&cfg, "engines use back").unwrap();
    assert_eq!(overlay(&cfg)["routing"], before["routing"]);
    set(&cfg, "engines use back").unwrap();
    assert_eq!(overlay(&cfg)["routing"], *saved);
    // A change after it leaves no setup active, until one equals it again.
    set(&cfg, "role judge codex").unwrap();
    assert!(cfg.engines().unwrap().contains("Active: unsaved"));
    set(&cfg, "role judge unset").unwrap();
    assert!(cfg.engines().unwrap().contains("Active: cheap"));
    // A setup naming a model no longer offered isn't put in force.
    set(&cfg, "engines use usual").unwrap();
    set(&cfg, "model triage claude unset").unwrap();
    set(&cfg, "engines use cheap").unwrap();
    set(&cfg, "model triage claude unset").unwrap();
    set(&cfg, "offer claude models").unwrap();
    let err = set(&cfg, "engines use usual").unwrap_err();
    assert!(err.contains("doesn't offer that model"), "{err}");
    for bad in [
        "engines use nothing",
        "engines use ../x",
        "engines save back",
        "engines save UP",
    ] {
        assert!(set(&cfg, bad).is_err(), "{bad}");
    }
}

#[test]
fn a_door_receipt_answers_again_without_switching_again() {
    let (d, cfg) = rig();
    set(&cfg, "role work claude").unwrap();
    set(&cfg, "engines save one").unwrap();
    set(&cfg, "role work codex").unwrap();
    set(&cfg, "engines save two").unwrap();
    let receipt = d.path().join("issue-1");
    let args = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let said = cfg
        .request(&receipt, &args(&["engines", "use", "one"]), "door")
        .unwrap();
    assert!(said.contains("Active: one"), "{said}");
    set(&cfg, "engines use two").unwrap();
    // A retried delivery of the same request can't undo the newer switch.
    let again = cfg
        .request(&receipt, &args(&["engines", "use", "one"]), "door")
        .unwrap();
    assert_eq!(again, said);
    assert!(cfg.engines().unwrap().contains("Active: two"));
}

#[test]
fn the_old_files_migrate_once() {
    let (_d, cfg) = rig();
    let routing = |engine: &str| Roles {
        roles: [(
            "triage".to_owned(),
            Route {
                engine: engine.into(),
                over: None,
            },
        )]
        .into(),
        models: [(
            "triage claude".to_owned(),
            Tuning {
                model: Some("small".into()),
                effort: Some("low".into()),
            },
        )]
        .into(),
    };
    let migration = |engine: &str| Migration {
        routing: routing(engine),
        offers: [(
            "claude".to_owned(),
            Offer {
                models: vec!["small".into()],
                efforts: vec!["low".into()],
            },
        )]
        .into(),
        setups: Setups {
            saved: [("usual".to_owned(), routing(engine))].into(),
            active: Some("usual".into()),
            previous: None,
        },
    };
    assert!(cfg.migrate(migration("codex"), "test").unwrap());
    let force = cfg.roles().unwrap();
    assert_eq!(force.route("triage", None).engine, "codex");
    assert_eq!(force.offers["claude"].models, ["small"]);
    assert!(cfg.engines().unwrap().contains("Active: usual"));
    // Never again, whatever the old files say now.
    assert!(!cfg.migrate(migration("claude"), "test").unwrap());
    assert_eq!(cfg.roles().unwrap().route("triage", None).engine, "codex");
    let log = fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
    assert_eq!(log.lines().count(), 1, "{log}");
    // A file that names a model its engine doesn't offer moves nothing.
    let (_d, fresh) = rig();
    let mut bad = migration("codex");
    bad.offers.clear();
    assert!(fresh.migrate(bad, "test").is_err());
    assert!(!fresh.overlay.exists());
}
