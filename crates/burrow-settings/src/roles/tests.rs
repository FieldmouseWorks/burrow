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
        ("claude", Some(80.0))
    );
    // A fraction is a threshold of its own, never rounded.
    set(&cfg, "role half codex over 80.5 claude").unwrap();
    let half = cfg.roles().unwrap().route("half", None).clone();
    assert_eq!(half.over.unwrap().pct, 80.5);
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
        "role fix codex over 100.5 claude",
        "role fix codex over NaN claude",
        "role fix codex over inf claude",
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
    set(&cfg, "offer claude models small").unwrap();
    let before = fs::read(cfg.roles_file()).unwrap();
    let err = set(&cfg, "model fix claude big-one").unwrap_err();
    assert!(err.contains("claude doesn't offer that model"), "{err}");
    let err = set(&cfg, "model fix claude - enormous").unwrap_err();
    assert!(err.contains("doesn't offer effort enormous"), "{err}");
    // Codex's efforts are its own: max is Claude's.
    assert!(set(&cfg, "model fix codex - max").is_err());
    assert_eq!(
        fs::read(cfg.roles_file()).unwrap(),
        before,
        "nothing changed"
    );

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
    fs::write(cfg.roles_file(), edited.to_string()).unwrap();
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
        receipts: BTreeMap::new(),
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
    assert!(!fresh.roles_file().exists());
}

#[test]
fn a_store_with_no_legacy_source_keeps_its_complete_accepted_state() {
    let (_d, cfg) = rig();
    set(&cfg, "role triage claude").unwrap();
    set(&cfg, "engines save mine").unwrap();
    let old = Migration {
        routing: Roles {
            roles: [
                ("triage".to_owned(), route(&["codex"]).unwrap()),
                ("fix".to_owned(), route(&["codex"]).unwrap()),
            ]
            .into(),
            models: BTreeMap::new(),
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
    assert_eq!(force.route("triage", None).engine, "claude", "newer wins");
    assert_eq!(
        force.route("fix", None).engine,
        "claude",
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
    set(&cfg, "role triage codex").unwrap();
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
        "role fix codex",
        "role fix unset",
        "role fix@Sam/Box unset",
        "role *@Sam/Box claude",
        "model fix codex - low",
        "model fix codex unset",
        "model fix@Sam/Box codex unset",
        "offer codex models",
        "offer claude efforts low",
        "engines save mine",
        "engines use mine",
        "engines use back",
    ] {
        let (d, mut cfg) = rig();
        let old = d.path().join("old");
        fs::create_dir(&old).unwrap();
        fs::write(old.join("roles"), "fix codex\nfix@Sam/Box codex\n").unwrap();
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
                roles: [("work".into(), route(&["codex"]).unwrap())].into(),
                ..Default::default()
            },
            receipts: [(receipt.to_string_lossy().into_owned(), said.into())].into(),
            ..Default::default()
        },
        "test",
    )
    .unwrap();
    set(&cfg, "role work claude").unwrap();
    let args = ["engines".into(), "use".into(), "back".into()];
    assert_eq!(cfg.request(&receipt, &args, "retry").unwrap(), said);
    assert_eq!(cfg.roles().unwrap().route("work", None).engine, "claude");
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
        "role work claude",
        "engines save one",
        "role work codex",
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
    assert_eq!(cfg.roles().unwrap().route("work", None).engine, "codex");
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

#[test]
fn the_deliberate_claude_roles_are_declared_routes_without_overflow() {
    let routes = default_routes();
    let help = crate::schema::find("proj.roles").unwrap().help;
    for (role, reason) in CLAUDE_ONLY {
        let route = &routes[*role];
        assert_eq!((route.engine.as_str(), &route.over), ("claude", &None));
        assert!(help.contains(reason), "{role}: the help states why");
    }
    assert_eq!(routes["planner"].engine, "codex");
    let (_dir, cfg) = rig();
    // Declared, then changed live: the next read has it.
    assert_eq!(cfg.roles().unwrap().route("update", None).engine, "claude");
    set(&cfg, "role update codex").unwrap();
    assert_eq!(cfg.roles().unwrap().route("update", None).engine, "codex");
}

#[test]
fn the_flakes_effort_is_a_starting_default_a_live_model_line_replaces() {
    let (_dir, mut cfg) = rig();
    cfg.claude_effort = Some("high".into());
    let tuned = |cfg: &Config, role: &str| cfg.roles().unwrap().tuning(role, None, "claude");
    assert_eq!(tuned(&cfg, "anything").effort.as_deref(), Some("high"));
    assert_eq!(
        cfg.roles().unwrap().tuning("anything", None, "codex"),
        Tuning::default()
    );
    set(&cfg, "model * claude - low").unwrap();
    assert_eq!(tuned(&cfg, "anything").effort.as_deref(), Some("low"));
    set(&cfg, "model * claude - -").unwrap();
    assert_eq!(tuned(&cfg, "anything"), Tuning::default());
    // One the engine doesn't offer is refused, not run.
    cfg.claude_effort = Some("enormous".into());
    assert!(cfg.roles().unwrap_err().contains("starting effort"));
}

#[test]
fn no_change_may_leave_the_settings_unreadable_with_the_flakes_effort() {
    let (_dir, mut cfg) = rig();
    cfg.claude_effort = Some("high".into());
    // Withdrawing the starting effort from the offer is refused, and
    // nothing is published.
    let err = set(&cfg, "offer claude efforts low").unwrap_err();
    assert!(err.contains("starting effort"), "{err}");
    cfg.roles().unwrap();
    set(&cfg, "offer claude efforts low high").unwrap();
    cfg.roles().unwrap();
}
