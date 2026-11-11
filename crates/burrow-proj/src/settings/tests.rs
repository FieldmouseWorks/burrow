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
                "autoland.rounds" => json!({"value": 3, "set": true}),
                "proj.pipelineSlots" => json!({"value": 1, "set": false}),
                "proj.maxJobsPerProject" => json!({"value": 2, "set": false}),
                // A module the machine doesn't have: its default holds.
                n if n.starts_with("autoland.reviewers.") => Value::Null,
                _ => json!({"value": s.default_value().unwrap(), "set": false}),
            };
            (s.name.to_owned(), entry)
        })
        .collect()
}

fn show(cfg: &Config) -> Value {
    serde_json::from_str(&command(cfg, &["show".into(), "--json".into()]).unwrap()).unwrap()
}

#[test]
fn every_option_shown_comes_from_the_schema() {
    let (d, mut cfg) = crate::usage::tests::rig();
    cfg.roles = d.path().join("config/roles");
    cfg.models = d.path().join("config/models");
    cfg.settings = tracked();

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
    assert_eq!(
        options["proj.roles"],
        json!({"value": {}, "origin": "default", "file": cfg.roles})
    );
    assert_eq!(options["proj.usageHold"]["value"], Value::Null);

    // Each live file, read with the parser its turns use.
    fs::create_dir_all(d.path().join("config/roles.d")).unwrap();
    fs::write(d.path().join("config/roles.d/cheap"), "triage codex\n").unwrap();
    fs::write(
        d.path().join("config/roles.d/usual"),
        "# usual\ntriage claude\nfix@Sam/Box   codex over 80 claude\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(d.path().join("config/roles.d/usual"), &cfg.roles).unwrap();
    fs::write(&cfg.models, "triage claude small low\nfix codex - high\n").unwrap();
    crate::usage::hold(&cfg, crate::now() + 3600, "save usage").unwrap();
    let doc = show(&cfg);
    let options = &doc["options"];
    assert_eq!(
        options["proj.roles"],
        json!({"value": {"triage": "claude", "fix@Sam/Box": "codex over 80 claude"},
               "origin": "live", "file": cfg.roles})
    );
    assert_eq!(
        options["proj.models"]["value"],
        json!({"triage claude": {"model": "small", "effort": "low"},
               "fix codex": {"model": null, "effort": "high"}})
    );
    assert_eq!(
        options["proj.engineSetups"]["value"],
        json!({"active": "usual", "saved": {
            "cheap": {"triage": "codex"},
            "usual": {"triage": "claude", "fix@Sam/Box": "codex over 80 claude"}}})
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
    cfg.settings.insert(
        "autoland.undeclared".into(),
        json!({"value": 1, "set": true}),
    );
    let err = command(&cfg, &["show".into()]).unwrap_err();
    assert!(err.starts_with("autoland.undeclared: "), "{err}");
    // So does one it declares that the flake doesn't render.
    cfg.settings = tracked();
    cfg.settings.remove("autoland.rounds");
    let err = command(&cfg, &["show".into()]).unwrap_err();
    assert!(err.starts_with("autoland.rounds: declared"), "{err}");
    // And a live file a turn would refuse.
    cfg.settings = tracked();
    fs::write(&cfg.models, "triage claude small enormous\n").unwrap();
    let err = command(&cfg, &["show".into()]).unwrap_err();
    assert!(err.contains("takes no effort enormous"), "{err}");
}

#[test]
fn the_schema_is_burrow_settings_own() {
    let (_d, cfg) = crate::usage::tests::rig();
    let got: Value = serde_json::from_str(&command(&cfg, &["schema".into()]).unwrap()).unwrap();
    assert_eq!(got, burrow_settings::schema::json_schema());
    assert!(command(&cfg, &["set".into()]).is_err());
}
