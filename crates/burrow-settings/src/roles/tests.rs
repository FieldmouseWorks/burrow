use super::*;
use crate::Config;
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

/// The role settings' document.
fn overlay(cfg: &Config) -> serde_json::Value {
    serde_json::from_slice(&fs::read(cfg.roles_file()).unwrap()).unwrap()
}

#[test]
fn a_role_with_nothing_set_runs_on_the_declared_defaults() {
    let (_d, cfg) = rig();
    let force = cfg.roles().unwrap();
    assert_eq!(force.route("triage", None).engine, "claude");
    assert_eq!(force.route("planner", Some("sam/box")).engine, "claude");
    // Its model is the declared `* ENGINE` entry, shown as one: null, the
    // engine's own.
    assert_eq!(force.models["* claude"], Tuning::default());
    assert_eq!(force.tuning("triage", None, "claude"), Tuning::default());
    assert!(force.offers["claude"].efforts.contains(&"max".to_owned()));
}

#[test]
fn the_closest_key_wins() {
    let (_d, cfg) = rig();
    let effort =
        |role: &str, repo: Option<&str>| cfg.roles().unwrap().tuning(role, repo, "claude").effort;
    set(&cfg, "model * claude - low").unwrap();
    set(&cfg, "model fix@Sam/Box claude - high").unwrap();
    set(&cfg, "model *@sam/web claude - medium").unwrap();
    assert_eq!(effort("other", None).as_deref(), Some("low"));
    assert_eq!(effort("other", Some("sam/web")).as_deref(), Some("medium"));
    assert_eq!(effort("fix", Some("sam/web")).as_deref(), Some("medium"));
    assert_eq!(effort("fix", Some("SAM/box")).as_deref(), Some("high"));
    assert_eq!(effort("fix", None).as_deref(), Some("low"));
    // The repo is stored folded, so its next setting replaces it.
    set(&cfg, "model fix@sam/box claude - max").unwrap();
    assert_eq!(effort("fix", Some("sam/box")).as_deref(), Some("max"));
    set(&cfg, "model fix@SAM/BOX claude unset").unwrap();
    assert_eq!(effort("fix", Some("sam/box")).as_deref(), Some("low"));
    assert!(
        !cfg.roles()
            .unwrap()
            .models
            .contains_key("fix@sam/box claude")
    );
    // Routes are keyed the same way.
    set(&cfg, "role fix@Sam/Box claude").unwrap();
    assert!(cfg.roles().unwrap().roles.contains_key("fix@sam/box"));
    set(&cfg, "role fix@SAM/BOX unset").unwrap();
    assert!(!cfg.roles().unwrap().roles.contains_key("fix@sam/box"));
    for bad in [
        "role fix gemini",
        "role fix codex",
        "role fix claude over 80 claude",
        "role fix@owner claude",
        "role fix",
    ] {
        assert!(set(&cfg, bad).is_err(), "{bad}");
    }
}

#[test]
fn an_unknown_model_or_effort_is_refused() {
    let (_d, cfg) = rig();
    set(&cfg, "offer claude models small").unwrap();
    let before = fs::read(cfg.roles_file()).unwrap();
    let err = set(&cfg, "model fix claude big-one").unwrap_err();
    assert!(err.contains("claude doesn't offer that model"), "{err}");
    let err = set(&cfg, "model fix claude - enormous").unwrap_err();
    assert!(err.contains("doesn't offer effort enormous"), "{err}");
    // An engine the machine doesn't run has nothing to offer.
    assert!(set(&cfg, "model fix codex - high").is_err());
    assert_eq!(
        fs::read(cfg.roles_file()).unwrap(),
        before,
        "nothing changed"
    );

    set(&cfg, "offer claude models big-one small").unwrap();
    set(&cfg, "model fix claude big-one high").unwrap();
    set(&cfg, "model judge claude - medium").unwrap();
    let force = cfg.roles().unwrap();
    assert_eq!(
        force.tuning("fix", Some("sam/box"), "claude"),
        Tuning {
            model: Some("big-one".into()),
            effort: Some("high".into())
        }
    );
    assert_eq!(
        force.tuning("judge", None, "claude").effort.as_deref(),
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
    fs::write(cfg.roles_file(), edited.to_string()).unwrap();
    assert!(cfg.roles().is_err());
}

#[test]
fn every_change_is_logged() {
    let (_d, cfg) = rig();
    set(&cfg, "offer claude models mid").unwrap();
    set(&cfg, "model triage claude mid low").unwrap();
    assert!(set(&cfg, "model triage claude other").is_err());
    let log = fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
    let lines: Vec<serde_json::Value> = log
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2, "a refused change isn't one: {log}");
    assert_eq!(
        lines[1]["change"],
        serde_json::json!(["model", "triage", "claude", "mid", "low"])
    );
    assert_eq!(lines[1]["actor"], "test");
}

#[test]
fn engines_use_changes_exactly_the_saved_values() {
    let (_d, cfg) = rig();
    set(&cfg, "offer claude models small big").unwrap();
    set(&cfg, "role triage claude").unwrap();
    set(&cfg, "model triage claude small low").unwrap();
    set(&cfg, "engines save cheap").unwrap();
    assert!(
        set(&cfg, "engines save cheap").is_err(),
        "save needs --replace"
    );
    set(&cfg, "role triage unset").unwrap();
    set(&cfg, "model fix claude - high").unwrap();
    set(&cfg, "model triage claude big").unwrap();
    let said = set(&cfg, "engines save usual").unwrap();
    assert!(said.contains("Active: usual"), "{said}");
    // What isn't a role setting: a pause, a scope, the offers.
    set(&cfg, "pause sam/box").unwrap();
    set(&cfg, &format!("pipeline {}", crate::now() + 600)).unwrap();
    let before = overlay(&cfg);
    let admission = fs::read(&cfg.overlay).unwrap();

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
    assert_eq!(
        fs::read(&cfg.overlay).unwrap(),
        admission,
        "admission unchanged"
    );
    let force = cfg.roles().unwrap();
    assert!(force.roles.contains_key("triage"));
    assert_eq!(
        force.tuning("fix", None, "claude"),
        Tuning::default(),
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
    set(&cfg, "role judge claude").unwrap();
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
    set(&cfg, "model work claude - low").unwrap();
    set(&cfg, "engines save one").unwrap();
    set(&cfg, "model work claude - high").unwrap();
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
    let routing = |effort: &str| Roles {
        roles: [(
            "triage".to_owned(),
            Route {
                engine: "claude".into(),
            },
        )]
        .into(),
        models: [(
            "triage claude".to_owned(),
            Tuning {
                model: Some("small".into()),
                effort: Some(effort.into()),
            },
        )]
        .into(),
    };
    let migration = |effort: &str| Migration {
        routing: routing(effort),
        offers: [(
            "claude".to_owned(),
            Offer {
                models: vec!["small".into()],
                efforts: vec!["low".into(), "high".into()],
            },
        )]
        .into(),
        receipts: BTreeMap::new(),
        setups: Setups {
            saved: [("usual".to_owned(), routing(effort))].into(),
            active: Some("usual".into()),
            previous: None,
        },
    };
    let effort = |cfg: &Config| cfg.roles().unwrap().tuning("triage", None, "claude").effort;
    assert!(cfg.migrate(migration("low"), "test").unwrap());
    let force = cfg.roles().unwrap();
    assert_eq!(effort(&cfg).as_deref(), Some("low"));
    assert_eq!(force.offers["claude"].models, ["small"]);
    assert!(cfg.engines().unwrap().contains("Active: usual"));
    // Never again, whatever the old files say now.
    assert!(!cfg.migrate(migration("high"), "test").unwrap());
    assert_eq!(effort(&cfg).as_deref(), Some("low"));
    let log = fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
    assert_eq!(log.lines().count(), 1, "{log}");
    // A file that names a model its engine doesn't offer moves nothing.
    let (_d, fresh) = rig();
    let mut bad = migration("low");
    bad.offers.clear();
    assert!(fresh.migrate(bad, "test").is_err());
    assert!(!fresh.roles_file().exists());
}

#[test]
fn a_store_with_no_legacy_source_keeps_its_complete_accepted_state() {
    let (_d, cfg) = rig();
    set(&cfg, "model triage claude - low").unwrap();
    set(&cfg, "engines save mine").unwrap();
    let high = Tuning {
        model: None,
        effort: Some("high".into()),
    };
    let old = Migration {
        routing: Roles {
            roles: [("fix".to_owned(), route(&["claude"]).unwrap())].into(),
            models: [
                ("triage claude".to_owned(), high.clone()),
                ("fix claude".to_owned(), high),
            ]
            .into(),
        },
        offers: BTreeMap::new(),
        receipts: BTreeMap::new(),
        setups: Setups {
            saved: [("old".to_owned(), Roles::default())].into(),
            active: None,
            previous: None,
        },
    };
    assert!(!cfg.migrate(old, "test").unwrap());
    let force = cfg.roles().unwrap();
    assert_eq!(
        force.tuning("triage", None, "claude").effort.as_deref(),
        Some("low"),
        "newer wins"
    );
    assert!(
        !force.roles.contains_key("fix") && !force.models.contains_key("fix claude"),
        "absent keys stay absent"
    );
    let engines = cfg.engines().unwrap();
    assert!(
        engines.contains("  mine\n") || engines.contains("* mine\n"),
        "{engines}"
    );
    assert!(!engines.contains("  old\n"), "{engines}");
}

#[test]
fn the_admission_overlay_keeps_its_own_format() {
    let (_d, cfg) = rig();
    set(&cfg, "pause sam/box").unwrap();
    set(&cfg, "role triage claude").unwrap();
    // A generation from before role settings reads this overlay: it takes
    // only these fields.
    let admission: serde_json::Value =
        serde_json::from_slice(&fs::read(&cfg.overlay).unwrap()).unwrap();
    let keys: Vec<_> = admission.as_object().unwrap().keys().cloned().collect();
    assert_eq!(keys, ["pipelineUntil", "receipts", "repos"]);
}

#[test]
fn every_role_mutation_waits_for_migration_but_admission_stays_available() {
    for command in [
        "role fix claude",
        "role fix unset",
        "role fix@Sam/Box unset",
        "role *@Sam/Box claude",
        "model fix claude - low",
        "model fix claude unset",
        "model fix@Sam/Box claude unset",
        "offer claude models",
        "offer claude efforts low",
        "engines save mine",
        "engines use mine",
        "engines use back",
    ] {
        let (d, mut cfg) = rig();
        let old = d.path().join("old");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("roles"), "fix claude\nfix@Sam/Box claude\n").unwrap();
        cfg.legacy_roles = Some(old);
        assert!(
            set(&cfg, command).unwrap_err().contains("await"),
            "{command}"
        );
        assert!(!cfg.roles_file().exists(), "{command}");
        set(&cfg, "pause sam/box").unwrap();
        assert!(!cfg.allows("sam/box").unwrap());
        cfg.migrate(Migration::default(), "test").unwrap();
        // The same unset/replacement is accepted after cutover, and a
        // migration retry cannot reinstate the removed value.
        if !command.contains("use") {
            set(&cfg, command).unwrap();
        }
        let before = fs::read(cfg.roles_file()).unwrap();
        cfg.migrate(Migration::default(), "test").unwrap();
        assert_eq!(fs::read(cfg.roles_file()).unwrap(), before);
    }
    let (d, mut cfg) = rig();
    cfg.legacy_roles = Some(d.path().join("not-created-yet"));
    assert!(
        set(&cfg, "role fix unset").is_err(),
        "even before old files appear"
    );
}

#[test]
fn broken_role_paths_never_supply_defaults() {
    use std::os::unix::fs::symlink;
    for kind in [
        "dangling",
        "dangling-parent",
        "file-parent",
        "denied",
        "loop",
    ] {
        let (d, mut cfg) = rig();
        let broken = d.path().join("broken");
        match kind {
            "dangling" => symlink(d.path().join("absent"), cfg.roles_file()).unwrap(),
            "dangling-parent" => {
                symlink(d.path().join("absent"), &broken).unwrap();
                cfg.overlay = broken.join("overlay.json");
            }
            "file-parent" => {
                fs::write(&broken, "file").unwrap();
                cfg.overlay = broken.join("overlay.json");
            }
            "denied" => {
                fs::create_dir(&broken).unwrap();
                burrow_test::permissions(&broken, 0o000);
                cfg.overlay = broken.join("overlay.json");
            }
            _ => {
                symlink(&broken, &broken).unwrap();
                cfg.overlay = broken.join("overlay.json");
            }
        }
        assert!(cfg.roles().is_err(), "{kind}");
        assert!(cfg.engines().is_err(), "{kind}");
        if kind == "denied" {
            burrow_test::permissions(&broken, 0o700);
        }
    }
    let (d, mut cfg) = rig();
    cfg.overlay = d.path().join("absent/also-absent/overlay.json");
    cfg.roles().unwrap();
}

#[test]
fn a_recovered_receipt_and_its_routing_are_one_durable_document() {
    let (d, cfg) = rig();
    let receipt = d.path().join("legacy.operation");
    let said = "Was active: two\nActive: one\n";
    cfg.migrate(
        Migration {
            routing: Roles {
                models: [(
                    "work claude".into(),
                    Tuning {
                        model: None,
                        effort: Some("low".into()),
                    },
                )]
                .into(),
                ..Default::default()
            },
            receipts: [(receipt.to_string_lossy().into_owned(), said.into())].into(),
            ..Default::default()
        },
        "test",
    )
    .unwrap();
    set(&cfg, "model work claude - high").unwrap();
    let args = ["engines".into(), "use".into(), "back".into()];
    assert_eq!(cfg.request(&receipt, &args, "retry").unwrap(), said);
    assert_eq!(
        cfg.roles()
            .unwrap()
            .tuning("work", None, "claude")
            .effort
            .as_deref(),
        Some("high")
    );
    // A visible post-rename receipt may not be acknowledged until the
    // publication's directory can be synced (including duplicate retries).
    burrow_test::permissions(d.path(), 0o300);
    assert!(cfg.request(&receipt, &args, "retry").is_err());
    assert!(cfg.legacy_answer(&receipt, None).is_err());
    burrow_test::permissions(d.path(), 0o700);
    assert_eq!(
        cfg.legacy_answer(&receipt, None).unwrap().as_deref(),
        Some(said)
    );
}

#[test]
fn new_requests_and_all_answer_lookups_share_one_receipt_identity() {
    let (d, cfg) = rig();
    let receipts = d.path().join("receipts");
    fs::create_dir(&receipts).unwrap();
    let alias = d.path().join("alias");
    std::os::unix::fs::symlink(&receipts, &alias).unwrap();
    for command in [
        "model work claude - low",
        "engines save one",
        "model work claude - high",
        "engines save two",
    ] {
        set(&cfg, command).unwrap();
    }
    let canonical = receipts.join("cycle.operation");
    let linked = alias.join("./cycle.operation");
    let answer = cfg
        .request(
            &linked,
            &["engines".into(), "use".into(), "one".into()],
            "test",
        )
        .unwrap();
    set(&cfg, "engines use two").unwrap();
    assert_eq!(
        cfg.request(
            &canonical,
            &["engines".into(), "use".into(), "back".into()],
            "retry"
        )
        .unwrap(),
        answer
    );
    assert_eq!(cfg.answered(&linked).unwrap(), Some(answer.clone()));
    assert_eq!(cfg.legacy_answer(&canonical, None).unwrap(), Some(answer));
    assert_eq!(
        cfg.roles()
            .unwrap()
            .tuning("work", None, "claude")
            .effort
            .as_deref(),
        Some("high")
    );
    let document = overlay(&cfg);
    assert_eq!(document["receipts"].as_object().unwrap().len(), 1);
    assert!(
        document["receipts"]
            .get(crate::receipt_key(&canonical).unwrap())
            .is_some()
    );
    assert!(!canonical.exists(), "no filesystem receipt is needed");
}

#[test]
fn imports_collapse_identical_receipt_aliases_and_refuse_conflicting_answers() {
    for conflicting in [false, true] {
        let (d, cfg) = rig();
        let alias = d.path().join("alias");
        std::os::unix::fs::symlink(d.path(), &alias).unwrap();
        let receipts = [
            (
                d.path().join("cycle").to_string_lossy().into_owned(),
                "original".into(),
            ),
            (
                alias.join("cycle").to_string_lossy().into_owned(),
                if conflicting { "different" } else { "original" }.into(),
            ),
        ]
        .into();
        let result = cfg.migrate(
            Migration {
                receipts,
                ..Default::default()
            },
            "test",
        );
        if conflicting {
            assert!(result.unwrap_err().contains("conflicting answers"));
            assert!(!cfg.roles_file().exists());
        } else {
            assert!(result.unwrap());
            assert_eq!(overlay(&cfg)["receipts"].as_object().unwrap().len(), 1);
            assert_eq!(
                cfg.legacy_answer(&alias.join("cycle"), None)
                    .unwrap()
                    .as_deref(),
                Some("original")
            );
        }
    }
}
