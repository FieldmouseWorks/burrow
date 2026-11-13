//! Every soft failure has an API-visible reporter, even when Actions makes
//! the job green. The step name is the wire format, not a parsed log warning.
use crate::harness::{steps, workflow};
use yaml_serde::Value;

fn unreported(workflow: &Value) -> Vec<String> {
    let mut bad = Vec::new();
    for (job, body) in workflow["jobs"].as_mapping().unwrap() {
        if body["steps"].is_null() {
            continue;
        }
        for step in steps(body) {
            if step["continue-on-error"].is_null() || step["continue-on-error"] == false {
                continue;
            }
            let id = step["id"].as_str().unwrap_or("");
            let key = step["name"].as_str().and_then(|n| n.strip_prefix("soft/"));
            let reported = key.is_some_and(|key| {
                !key.is_empty()
                    && steps(body).iter().any(|s| {
                        s["name"] == format!("degraded/{key}")
                            && s["if"]
                                == format!("${{{{ always() && steps.{id}.outcome == 'failure' }}}}")
                            && s["run"].as_str().is_some_and(|r| {
                                r.contains(&format!("degraded/{key}"))
                                    && r.contains("GITHUB_STEP_SUMMARY")
                            })
                    })
            });
            if id.is_empty() || !reported {
                bad.push(format!("{job:?}/{id}"));
            }
        }
    }
    bad
}

#[test]
fn soft_steps_have_degradation_reporters() {
    let dir = crate::harness::repo_file(".github/workflows");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if !matches!(
            path.extension().and_then(|s| s.to_str()),
            Some("yml" | "yaml")
        ) {
            continue;
        }
        let value = crate::harness::yaml(&std::fs::read_to_string(&path).unwrap());
        assert!(
            unreported(&value).is_empty(),
            "{}: {:?}",
            path.display(),
            unreported(&value)
        );
    }
}

#[test]
fn refuses_the_original_keep_builds_steps() {
    let mut old = workflow(".github/workflows/flake-check.yml");
    for step in old["jobs"]["check"]["steps"].as_sequence_mut().unwrap() {
        if step["continue-on-error"] == true {
            step.as_mapping_mut()
                .unwrap()
                .remove(Value::String("name".into()));
        }
    }
    assert_eq!(unreported(&old).len(), 4);
}

#[test]
fn reporters_distinguish_soft_failure_cache_miss_and_extraction_failure() {
    use crate::expr::{Ctx, V, value};
    use crate::harness::{steps, text};
    let flow = workflow(".github/workflows/flake-check.yml");
    let job = &flow["jobs"]["check"];
    for (id, key) in [
        ("keep-tool", "keep-tool"),
        ("keep", "keep-builds"),
        ("keep-save", "keep-save"),
    ] {
        let reporter = steps(job)
            .iter()
            .find(|s| s["name"] == format!("degraded/{key}"))
            .unwrap();
        for (outcome, expected) in [("failure", true), ("success", false), ("skipped", false)] {
            let c: Ctx = [
                ("always()".into(), true.into()),
                (format!("steps.{id}.outcome"), V::from(outcome)),
            ]
            .into_iter()
            .collect();
            assert_eq!(value(text(&reporter["if"]), &c).truthy(), expected);
        }
    }
    let reporter = steps(job)
        .iter()
        .find(|s| s["name"] == "degraded/kept-restore")
        .unwrap();
    for (lookup, restored, degraded) in [
        ("", "", false),
        ("false", "", true),
        ("true", "", true),
        ("false", "false", false),
        ("true", "true", false),
    ] {
        let c: Ctx = [
            ("always()".into(), true.into()),
            (
                "steps.kept-lookup.outputs.cache-hit".into(),
                V::from(lookup),
            ),
            (
                "steps.kept-restore.outputs.cache-hit".into(),
                V::from(restored),
            ),
        ]
        .into_iter()
        .collect();
        assert_eq!(value(text(&reporter["if"]), &c).truthy(), degraded);
    }
    let lookup = crate::harness::step(job, "kept-lookup");
    let restore = crate::harness::step(job, "kept-restore");
    assert_eq!(lookup["if"], restore["if"]);
    assert_eq!(lookup["with"]["lookup-only"], true);
    for field in ["key", "restore-keys", "path"] {
        assert_eq!(lookup["with"][field], restore["with"][field]);
    }
    let at = |id: &str| steps(job).iter().position(|s| s["id"] == id).unwrap();
    assert!(at("kept-lookup") < at("kept-restore"));
    assert!(at("keep-save") < at("saved-lookup"));
}

#[test]
fn cache_save_warning_is_reported_when_no_entry_was_saved() {
    use crate::expr::{Ctx, value};
    use crate::harness::{steps, text};
    let flow = workflow(".github/workflows/flake-check.yml");
    let reporter = steps(&flow["jobs"]["check"])
        .iter()
        .find(|s| s["name"] == "degraded/keep-save-missing")
        .unwrap();
    for (outcome, hit, expected) in [
        ("success", "", true),
        ("success", "false", true),
        ("success", "true", false),
        ("skipped", "", false),
    ] {
        let c: Ctx = [
            ("always()".into(), true.into()),
            ("steps.saved-lookup.outcome".into(), outcome.into()),
            ("steps.saved-lookup.outputs.cache-hit".into(), hit.into()),
        ]
        .into_iter()
        .collect();
        assert_eq!(value(text(&reporter["if"]), &c).truthy(), expected);
    }
    let dir = tempfile::tempdir().unwrap();
    let summary = dir.path().join("summary");
    let out = std::process::Command::new("bash")
        .args(["-e", "-c", text(&reporter["run"])])
        .env("GITHUB_STEP_SUMMARY", &summary)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        std::fs::read_to_string(summary).unwrap(),
        "degraded/keep-save-missing\n"
    );
}
