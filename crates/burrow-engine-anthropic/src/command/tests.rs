use super::*;
use std::fs;

#[test]
fn a_claude_turn_takes_its_model_and_effort() {
    let opts = RunArgs::default();
    let plain = run_args(&opts);
    assert!(!plain.contains(&"--model".into()) && !plain.contains(&"--effort".into()));
    assert!(plain.contains(&"stream-json".into()));
    let opts = RunArgs {
        model: Some("chosen".into()),
        effort: Some("medium".into()),
        session: Some("session".into()),
        max_budget_usd: Some(10_000_000),
        append_system_prompt: Some("extra".into()),
    };
    let given = run_args(&opts).join(" ");
    assert!(
        given.contains("--effort medium") && given.contains("--model chosen"),
        "{given}"
    );
    assert!(given.contains("--resume session"), "{given}");
    assert!(given.contains("--max-budget-usd 10.000000"), "{given}");
    assert!(given.contains("--append-system-prompt extra"), "{given}");
}

#[test]
fn claude_flags_renders_settings_writable_dirs_and_mcp_servers() {
    let dir = tempfile::tempdir().unwrap();
    let writable = dir.path().join("writable");
    let readonly = dir.path().join("readonly");
    let missing = dir.path().join("missing");
    fs::create_dir_all(&writable).unwrap();
    fs::create_dir_all(&readonly).unwrap();

    let grants = Grants {
        env: BTreeMap::from([("NIX_REMOTE".into(), "daemon".into())]),
        inherit_environment: true,
        paths: vec![
            (writable.clone(), true),
            (readonly.clone(), false),
            (missing.clone(), true),
        ],
        servers: BTreeMap::from([(
            "machine.ledger".into(),
            McpServer {
                command: "/bin/server".into(),
                args: vec!["--stdio".into()],
                env: BTreeMap::from([("SETTING".into(), "value".into())]),
            },
        )]),
    };

    let flags = claude_flags(&grants);
    assert_eq!(flags[0], "--settings");
    let settings: serde_json::Value = serde_json::from_str(&flags[1]).unwrap();
    assert_eq!(settings["env"]["NIX_REMOTE"], "daemon");
    assert!(flags.contains(&writable.to_string_lossy().into_owned()));
    assert!(!flags.contains(&readonly.to_string_lossy().into_owned()));
    assert!(!flags.contains(&missing.to_string_lossy().into_owned()));
    assert!(flags.contains(&"--strict-mcp-config".to_owned()));
    let config: serde_json::Value = serde_json::from_str(flags.last().unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"]["machine.ledger"]["command"],
        "/bin/server"
    );
    assert_eq!(
        config["mcpServers"]["machine.ledger"]["env"]["SETTING"],
        "value"
    );
}
