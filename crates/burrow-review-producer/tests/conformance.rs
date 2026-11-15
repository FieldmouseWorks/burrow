//! The review plugin passes Conary's own `cn plugin check`
//! (`Plugin::check`), run as the flow runs it: an executable with no
//! arguments, which the module makes a one-line wrapper of
//! `burrow-review-plugin plugin CONFIG`. The fixture needs no producer:
//! none listens here.
use conary_core::plugin::{Kind, Limits, Plugin};
use std::time::Duration;

#[test]
fn the_plugin_passes_conarys_check_with_no_producer() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let overlay = d.join("overlay.json");
    std::fs::write(
        overlay.with_extension("roles.json"),
        serde_json::json!({"offers": {"codex": {"models": ["m-1"], "efforts": ["high"]}},
            "migrated": 1})
        .to_string(),
    )
    .unwrap();
    let config = d.join("review.json");
    let cfg = serde_json::json!({
        "socket": d.join("none.sock"), "controller": "flow",
        "producer": "machine-review", "credential": "review-key", "role": "codex",
        "flow": d.join("flow"), "settings": {"overlay": overlay},
        "upkeep": d.join("upkeep.json"), "gh": "gh", "state": d.join("state"),
        "fresh": d.join("fresh"), "queue": d.join("queue"), "done": d.join("done"),
        "sign": d.join("sign"), "keep": 86400,
    });
    std::fs::write(&config, cfg.to_string()).unwrap();
    let plugin = d.join("burrow-review-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\nexec {} plugin {}\n",
            env!("CARGO_BIN_EXE_burrow-review-plugin"),
            config.display()
        ),
    );
    let limits = Limits {
        deadline: Duration::from_secs(30),
        response_bytes: 1 << 20,
    };
    let runner = Plugin::describe(&plugin, limits).expect("it describes itself");
    let kinds = runner.check().expect("it passes Conary's check");
    assert_eq!(kinds, [Kind::Review]);
}
