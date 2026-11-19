//! The judge plugin passes Conary's own `cn plugin check`
//! (`Plugin::check`), run as the flow runs it: an executable with no
//! arguments, which the module makes a one-line wrapper of
//! `burrow-judge plugin CONFIG`. The fixture needs no producer: none
//! listens here.
use conary_core::plugin::{Kind, Limits, Plugin};
use std::time::Duration;

#[test]
fn the_plugin_passes_conarys_check_with_no_producer() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let overlay = d.join("overlay.json");
    std::fs::write(
        overlay.with_extension("roles.json"),
        serde_json::json!({
            "routing": {"models": {"judge claude": {"model": "m-1", "effort": "high"}}},
            "offers": {"claude": {"models": ["m-1"], "efforts": ["high"]}}, "migrated": 1})
        .to_string(),
    )
    .unwrap();
    let config = d.join("judge.json");
    let cfg = serde_json::json!({
        "socket": d.join("none.sock"), "controller": "flow",
        "runner": d.join("runner.sock"), "user": "autoland-judge",
        "producer": "machine-judge", "credential": "judge-key", "seal": "judge-seal",
        "flow": d.join("flow"), "settings": {"overlay": overlay},
        "proj": ["proj"], "jobs": d.join("jobs"), "role": "judge", "app": null,
        "timeout": "60m", "idle": "20m", "owner": "Sam",
        "api": "https://api.github.com", "token": "true", "runner_token": "true",
        "gh": "gh", "state": d.join("state"), "runner_state": d.join("runner"),
    });
    std::fs::write(&config, cfg.to_string()).unwrap();
    let plugin = d.join("burrow-judge-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\nexec {} plugin {}\n",
            env!("CARGO_BIN_EXE_burrow-judge"),
            config.display()
        ),
    );
    let limits = Limits {
        deadline: Duration::from_secs(30),
        response_bytes: 1 << 20,
    };
    let runner = Plugin::describe(&plugin, limits).expect("it describes itself");
    let kinds = runner.check().expect("it passes Conary's check");
    assert_eq!(kinds, [Kind::Judge]);
}
