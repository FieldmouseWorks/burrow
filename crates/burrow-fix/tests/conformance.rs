//! The plugin passes Conary's own `cn plugin check` (`Plugin::check`), run
//! as the flow runs it: an executable with no arguments, which the module
//! makes a one-line wrapper of `burrow-fix plugin CONFIG`. The fixtures
//! need no broker: none listens here.
use conary_core::plugin::{Kind, Limits, Plugin};
use std::time::Duration;

#[test]
fn the_plugin_passes_conarys_check_with_no_broker() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("fix.json");
    let cfg = serde_json::json!({
        "socket": dir.path().join("none.sock"),
        "controller": "flow",
        "settings": {"overlay": dir.path().join("overlay.json")},
        "proj": ["proj"],
        "jobs": dir.path().join("jobs"),
        "role": "fix",
        "app": null,
        "timeout": "60m",
        "idle": "20m",
        "gh": "gh",
        "state": dir.path().join("state"),
    });
    std::fs::write(&config, cfg.to_string()).unwrap();
    // The fixing role on claude, which offers one model.
    std::fs::write(
        dir.path().join("overlay.roles.json"),
        serde_json::json!({"offers": {"claude": {"models": ["a-model"], "efforts": ["high"]}}, "migrated": 1})
            .to_string(),
    )
    .unwrap();
    let plugin = dir.path().join("burrow-fix-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\nexec {} plugin {}\n",
            env!("CARGO_BIN_EXE_burrow-fix"),
            config.display()
        ),
    );
    let limits = Limits {
        deadline: Duration::from_secs(30),
        response_bytes: 1 << 20,
    };
    let runner = Plugin::describe(&plugin, limits).expect("it describes itself");
    assert_eq!(
        runner.check().expect("it passes Conary's check"),
        [Kind::Fix]
    );
}
