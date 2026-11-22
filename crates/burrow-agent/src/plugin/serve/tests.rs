use super::*;

fn describe(sandbox: &str, v2_kinds: serde_json::Value) -> String {
    serde_json::json!({
        "contract": "conary-plugin v1",
        "by": {"engine": "example", "session": null},
        "kind": "describe",
        "versions": ["conary-plugin v1", "conary-plugin v5"],
        "kinds": [],
        "v2_kinds": v2_kinds,
        "models": [],
        "capabilities": {"sandbox": sandbox, "network": true, "devices": ["kvm"], "mcp": false},
        "meters": [],
    })
    .to_string()
}

#[test]
fn a_job_runs_here_for_every_engine_without_a_sandbox_of_its_own() {
    for (sandbox, runs) in [("none", true), ("needs", true), ("brings", false)] {
        let (kinds, offered) = declared(&describe(sandbox, serde_json::json!(["job"]))).unwrap();
        assert_eq!(runs_job(&kinds, &offered), Ok(runs), "{sandbox}");
    }
    let (kinds, offered) = declared(&describe("needs", serde_json::json!([]))).unwrap();
    assert!(runs_job(&kinds, &offered).is_err());
}

#[test]
fn grants_are_computed_only_from_a_describe_that_reads() {
    let good = describe("needs", serde_json::json!(["job"]));
    let (kinds, offered) = declared(&good).unwrap();
    assert_eq!(kinds, [Kind::Job]);
    assert!(offered.network);
    assert!(declared("not json").is_err());
    let mut other: serde_json::Value = serde_json::from_str(&good).unwrap();
    other["contract"] = "conary-plugin v5".into();
    assert!(declared(&other.to_string()).is_err());
    let mut silent: serde_json::Value = serde_json::from_str(&good).unwrap();
    silent["capabilities"] = serde_json::Value::Null;
    assert!(declared(&silent.to_string()).is_err());
}
