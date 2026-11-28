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
fn refuses_unreported_soft_steps() {
    let mut old = workflow(".github/workflows/flake-check.yml");
    for step in old["jobs"]["check"]["steps"].as_sequence_mut().unwrap() {
        if step["continue-on-error"] == true {
            step.as_mapping_mut()
                .unwrap()
                .remove(Value::String("name".into()));
        }
    }
    // The build store's probe (`soft/builds`), upload (`soft/builds-write`),
    // and mid-run fallback check (`soft/builds-fallback`).
    assert_eq!(unreported(&old).len(), 3);
}

#[test]
fn reporters_fire_only_when_their_soft_step_fails() {
    use crate::expr::{Ctx, V, value};
    use crate::harness::{steps, text};
    let flow = workflow(".github/workflows/flake-check.yml");
    for (job_name, id, key) in [
        ("reuse", "builds", "builds"),
        ("reuse", "lookup", "gate-reuse"),
        ("check", "builds", "builds"),
        ("check", "builds-write", "builds-write"),
        ("check", "builds-fallback", "builds-fallback"),
        ("docs", "builds", "builds"),
        ("docs", "builds-fallback", "builds-fallback"),
        ("negative", "builds", "builds"),
        ("negative", "builds-fallback", "builds-fallback"),
    ] {
        let job = &flow["jobs"][job_name];
        let reporter = steps(job)
            .iter()
            .find(|s| s["name"] == format!("degraded/{key}"))
            .unwrap_or_else(|| panic!("{job_name}: no degraded/{key}"));
        for (outcome, expected) in [("failure", true), ("success", false), ("skipped", false)] {
            let c: Ctx = [
                ("always()".into(), true.into()),
                (format!("steps.{id}.outcome"), V::from(outcome)),
            ]
            .into_iter()
            .collect();
            assert_eq!(
                value(text(&reporter["if"]), &c).truthy(),
                expected,
                "{job_name}/{key} {outcome}"
            );
        }
    }
    // No kept-builds degradations remain anywhere in the workflow.
    let raw = std::fs::read_to_string(crate::harness::repo_file(
        ".github/workflows/flake-check.yml",
    ))
    .unwrap();
    for gone in [
        "degraded/kept-",
        "degraded/keep-",
        "soft/kept-",
        "soft/keep-",
    ] {
        assert!(!raw.contains(gone), "{gone} still in flake-check.yml");
    }
}
