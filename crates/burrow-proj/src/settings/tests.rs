use std::fs;

use burrow_settings::schema::{Change, SETTINGS};
use serde_json::{Value, json};

use super::*;

/// The flake's values as tracked.nix renders them on a machine that sets
/// nothing, but the steward's rounds.
fn tracked() -> serde_json::Map<String, Value> {
    SETTINGS
        .iter()
        .filter(|s| s.change == Change::Deploy)
        .map(|s| {
            let entry = match s.name {
                "autoland.rounds" => json!({"value": 3, "set": true, "default": 5}),
                "proj.pipelineSlots" => json!({"value": 1, "set": false, "default": 1}),
                "proj.maxJobsPerProject" => json!({"value": 2, "set": false, "default": 2}),
                // A module the machine doesn't have: its default holds.
                n if n.starts_with("autoland.reviewers.") => Value::Null,
                _ => {
                    let d = s.default_value().unwrap();
                    json!({"value": d, "set": false, "default": d})
                }
            };
            (s.name.to_owned(), entry)
        })
        .collect()
}

/// Puts the flake's values where the config says, as /etc has them.
fn set(cfg: &mut Config, tracked: serde_json::Map<String, Value>) {
    let file = cfg.jobs.join("settings.json");
    fs::write(&file, Value::Object(tracked).to_string()).unwrap();
    cfg.tracked_settings = Some(file);
}

fn show(cfg: &Config) -> Value {
    serde_json::from_str(&command(cfg, &["show".into(), "--json".into()]).unwrap()).unwrap()
}

#[test]
fn every_option_shown_comes_from_the_schema() {
    let (d, mut cfg) = crate::usage::tests::rig();
    let overlay = d.path().join("settings/overlay.json");
    cfg.settings.as_mut().unwrap().overlay = overlay.clone();
    let roles_file = cfg.settings.as_ref().unwrap().roles_file();
    set(&mut cfg, tracked());

    // Nothing live yet: every live option at its default, naming its file.
    let doc = show(&cfg);
    let options = doc["options"].as_object().unwrap();
    let shown: Vec<_> = options.keys().map(String::as_str).collect();
    let mut declared: Vec<_> = SETTINGS.iter().map(|s| s.name).collect();
    declared.sort_unstable();
    assert_eq!(shown, declared);
    assert_eq!(
        options["autoland.rounds"],
        json!({"value": 3, "origin": "flake"})
    );
    assert_eq!(
        options["autoland.reviewers.codex.effort"],
        json!({"value": "high", "origin": "default"})
    );
    // A role with nothing set shows the declared default, never an
    // implicit one: every role on Claude but the planner, each on its
    // engine's own model.
    let unset = json!({"engine": "claude", "over": null});
    assert_eq!(options["proj.roles"]["value"]["*"], unset);
    assert_eq!(options["proj.roles"]["origin"], "default");
    assert_eq!(options["proj.roles"]["file"], json!(roles_file));
    assert_eq!(
        options["proj.models"]["value"]["* claude"],
        json!({"model": null, "effort": null})
    );
    assert_eq!(
        options["proj.engineOffers"]["value"]["claude"]["models"],
        json!([])
    );
    assert_eq!(options["proj.usageHold"]["value"], Value::Null);

    // Each live value, set through the door and read as its turns read it.
    crate::config::tests::set(
        &mut cfg,
        d.path(),
        &[
            "role triage codex",
            "engines save cheap",
            "role triage claude",
            "role fix@Sam/Box codex over 80 claude",
            "offer claude models small",
            "model triage claude small low",
            "model fix codex - high",
            "engines save usual",
        ],
    );
    crate::usage::hold(&cfg, crate::now() + 3600, "save usage").unwrap();
    let doc = show(&cfg);
    let options = &doc["options"];
    let usual_roles = json!({
        "*": {"engine": "claude", "over": null},
        "planner": {"engine": "codex", "over": null},
        "triage": {"engine": "claude", "over": null},
        "fix@sam/box": {"engine": "codex", "over": {"pct": 80.0, "engine": "claude"}},
    });
    assert_eq!(
        options["proj.roles"],
        json!({"value": usual_roles, "origin": "live", "file": roles_file})
    );
    assert_eq!(
        options["proj.models"]["value"],
        json!({"* claude": {"model": null, "effort": null},
               "* codex": {"model": null, "effort": null},
               "triage claude": {"model": "small", "effort": "low"},
               "fix codex": {"model": null, "effort": "high"}})
    );
    assert_eq!(
        options["proj.engineOffers"]["value"]["claude"]["models"],
        json!(["small"])
    );
    assert_eq!(options["proj.engineSetups"]["value"]["active"], "usual");
    assert_eq!(
        options["proj.engineSetups"]["value"]["saved"]["cheap"],
        json!({"roles": {"triage": {"engine": "codex", "over": null}}, "models": {}})
    );
    let hold = &options["proj.usageHold"];
    assert_eq!(hold["origin"], "live");
    assert_eq!(hold["value"]["owner"], true);
    assert_eq!(hold["value"]["reason"], "save usage");
    let text = command(&cfg, &[]).unwrap();
    assert_eq!(text.lines().count(), SETTINGS.len());
    assert!(
        text.lines()
            .any(|l| l.starts_with("autoland.rounds ")
                && l.ends_with("  3  (flake, deploy to change)")),
        "{text}"
    );

    // The flake rendering an option the schema doesn't declare fails it.
    let mut extra = tracked();
    extra.insert(
        "autoland.undeclared".into(),
        json!({"value": 1, "set": true, "default": 1}),
    );
    set(&mut cfg, extra);
    let err = command(&cfg, &["show".into()]).unwrap_err();
    assert!(err.starts_with("autoland.undeclared: "), "{err}");
    // So does one it declares that the flake doesn't render.
    let mut missing = tracked();
    missing.remove("autoland.rounds");
    set(&mut cfg, missing);
    let err = command(&cfg, &["show".into()]).unwrap_err();
    assert!(err.starts_with("autoland.rounds: declared"), "{err}");
    // And a usage record that can't be read is no "no hold".
    set(&mut cfg, tracked());
    fs::write(cfg.jobs.join(crate::usage::FILE), "{not json").unwrap();
    let err = command(&cfg, &["show".into()]).unwrap_err();
    assert!(err.contains(".usage.json"), "{err}");
    fs::remove_file(cfg.jobs.join(crate::usage::FILE)).unwrap();
    // And an overlay a turn would refuse.
    set(&mut cfg, tracked());
    let mut edited: Value = serde_json::from_slice(&fs::read(&roles_file).unwrap()).unwrap();
    edited["routing"]["models"]["triage claude"]["effort"] = json!("enormous");
    fs::write(&roles_file, edited.to_string()).unwrap();
    let err = command(&cfg, &["show".into()]).unwrap_err();
    assert!(err.contains("doesn't offer effort enormous"), "{err}");
}

#[test]
fn the_schema_is_burrow_settings_own() {
    let (_d, cfg) = crate::usage::tests::rig();
    let got: Value = serde_json::from_str(&command(&cfg, &["schema".into()]).unwrap()).unwrap();
    assert_eq!(got, burrow_settings::schema::json_schema());
    assert!(command(&cfg, &["set".into()]).is_err());
}

#[test]
fn admission_reads_the_one_overlay_and_writes_without_a_tracked_document() {
    let (d, mut cfg) = crate::usage::tests::rig();
    let policy = burrow_settings::Config {
        overlay: d.path().join("overlay.json"),
        allowed: vec!["sam/kept".into()],
        pipeline_repos: vec!["sam/pipeline".into()],
    };
    cfg.settings = Some(policy.clone());
    cfg.tracked_settings = None;
    command(&cfg, &["pause".into(), "sam/kept".into()]).unwrap();
    assert!(!policy.allows("sam/kept").unwrap());
    set(&mut cfg, tracked());
    let until = crate::now() + 3600;
    command(&cfg, &["pipeline".into(), until.to_string()]).unwrap();
    let shown = show(&cfg);
    assert_eq!(
        shown["options"]["settings.repositoryOverrides"]["value"],
        json!({"sam/kept": false})
    );
    assert_eq!(shown["options"]["settings.pipelineUntil"]["value"], until);
    assert_eq!(
        shown["options"]["settings.repositoryOverrides"]["file"],
        json!(policy.overlay)
    );
    fs::write(&policy.overlay, "broken").unwrap();
    assert!(command(&cfg, &["show".into()]).is_err());
}
