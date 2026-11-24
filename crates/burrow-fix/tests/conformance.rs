//! The plugin passes Conary's own `cn plugin check` (`Plugin::check`), run
//! as the flow runs it: an executable with no arguments, which the module
//! makes a one-line wrapper of `burrow-fix plugin CONFIG`. The fixtures
//! need no broker: none listens here. So does the stuck role's job plugin
//! (`burrow-fix job CONFIG`), with `job` advertised.
use conary_core::plugin::{Kind, Limits, Plugin};
use std::path::Path;
use std::time::Duration;

#[test]
fn the_plugin_passes_conarys_check_with_no_broker() {
    let dir = tempfile::tempdir().unwrap();
    let runner = described(dir.path(), "plugin", None);
    assert_eq!(
        runner.check().expect("it passes Conary's check"),
        [Kind::Fix]
    );
}

#[test]
fn the_job_plugin_passes_conarys_check_with_job_advertised() {
    let dir = tempfile::tempdir().unwrap();
    let runner = described(dir.path(), "job", Some("stuck"));
    assert_eq!(runner.kinds(), [Kind::Job]);
    assert_eq!(
        runner.check().expect("it passes Conary's check"),
        [Kind::Job]
    );
}

/// The runner the flow holds for `burrow-fix VERB`, with the stuck role
/// `stuck` configured or none.
fn described(dir: &Path, verb: &str, stuck: Option<&str>) -> Plugin {
    let config = dir.join("fix.json");
    let mut cfg = serde_json::json!({
        "socket": dir.join("none.sock"),
        "controller": "flow",
        "settings": {"overlay": dir.join("overlay.json")},
        "proj": ["proj"],
        "jobs": dir.join("jobs"),
        "role": "fix",
        "app": null,
        "timeout": "60m",
        "idle": "20m",
        "gh": "gh",
        "state": dir.join("state"),
    });
    if let Some(role) = stuck {
        cfg["stuck"] = role.into();
    }
    std::fs::write(&config, cfg.to_string()).unwrap();
    // Every role on claude, which offers one model.
    std::fs::write(
        dir.join("overlay.roles.json"),
        serde_json::json!({"offers": {"claude": {"models": ["a-model"], "efforts": ["high"]}}, "migrated": 1})
            .to_string(),
    )
    .unwrap();
    let plugin = dir.join("burrow-fix-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\nexec {} {verb} {}\n",
            burrow_test::program!("burrow-fix"),
            config.display()
        ),
    );
    let limits = Limits {
        deadline: Duration::from_secs(30),
        response_bytes: 1 << 20,
    };
    Plugin::describe(&plugin, limits).expect("it describes itself")
}
