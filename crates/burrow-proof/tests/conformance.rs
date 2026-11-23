//! The proof plugin passes Conary's own `cn plugin check` (`Plugin::check`),
//! run as the flow runs it: an executable with no arguments, which the
//! module makes a one-line wrapper of `burrow-proof-plugin plugin CONFIG`.
//! The fixtures need no producer: none listens here.
use conary_core::plugin::{Kind, Limits, Plugin};
use std::time::Duration;

#[test]
fn the_plugin_passes_conarys_check_with_no_producer() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let config = d.join("proof.json");
    let cfg = serde_json::json!({
        "socket": d.join("none.sock"), "controller": "flow",
        "producer": "machine-proof", "credential": "proof-key",
        "flow": d.join("flow"), "gh": "gh", "git": "git",
        "github": "https://github.com", "state": d.join("state"),
        "unit": "autoland-proof-work",
        "systemctl": "systemctl", "timeout": 600, "nix": "/nix/bin",
        "heavy": null, "trees": d.join("trees"),
    });
    std::fs::write(&config, cfg.to_string()).unwrap();
    let plugin = d.join("burrow-proof-plugin");
    burrow_test::executable(
        &plugin,
        format!(
            "#!/bin/sh\nexec {} plugin {}\n",
            burrow_test::program!("burrow-proof-plugin"),
            config.display()
        ),
    );
    let limits = Limits {
        deadline: Duration::from_secs(30),
        response_bytes: 1 << 20,
    };
    let runner = Plugin::describe(&plugin, limits).expect("it describes itself");
    let mut kinds = runner.check().expect("it passes Conary's check");
    kinds.sort();
    assert_eq!(kinds, [Kind::Proof, Kind::Prepare]);
}
