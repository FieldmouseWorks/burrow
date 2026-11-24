//! Every job of the shared gate's workflow has a limit of its own, and one
//! that runs the gate or the negative leaves its run room to stop itself
//! first (#1488). A job with none takes GitHub's six hours, and one still
//! going holds its run's failed jobs from being run again (burrow-upkeep's
//! ci.rs waits for a run to end).
use crate::harness::{read, repo_file, steps, workflow};
use yaml_serde::Value;

/// No job of it takes longer than this: its longest, a leg, is its run's
/// limit with the steps after it.
const CEILING: u64 = 240;
/// What a job's steps other than its run and those with limits of their
/// own may take: checkout, restores, installing Nix, signing, uploads.
const OVERHEAD: u64 = 30;

/// The run's own limit, in minutes, from where burrow-flake-check says it.
fn run_minutes() -> u64 {
    let main = read(&repo_file("crates/burrow-flake-check/src/main.rs"));
    let line = main
        .lines()
        .find_map(|l| l.strip_prefix("const RUN_SECONDS: u32 = "))
        .expect("burrow-flake-check's RUN_SECONDS");
    let minutes = line
        .strip_suffix(" * 60;")
        .expect("RUN_SECONDS as minutes * 60");
    minutes.parse().expect("RUN_SECONDS's minutes")
}

/// Each job without a limit, past the ceiling, or that gives its run less
/// than the run's own limit with its other steps.
fn unbounded(workflow: &Value, run: u64) -> Vec<String> {
    let mut bad = Vec::new();
    for (job, body) in workflow["jobs"].as_mapping().expect("jobs") {
        let job = job.as_str().unwrap_or_default();
        let Some(limit) = body["timeout-minutes"].as_u64() else {
            bad.push(format!("{job}: no timeout-minutes of its own"));
            continue;
        };
        if limit > CEILING {
            bad.push(format!("{job}: {limit} minutes, past {CEILING}"));
        }
        let runs = steps(body).iter().any(|s| {
            s["run"]
                .as_str()
                .is_some_and(|r| r.contains("$TOOLS/gate.sh") || r.contains("$TOOLS/negative.sh"))
        });
        let others: u64 = steps(body)
            .iter()
            .filter_map(|s| s["timeout-minutes"].as_u64())
            .sum();
        if runs && limit < run + others + OVERHEAD {
            bad.push(format!(
                "{job}: {limit} minutes, under its run's {run}, its steps' {others} and {OVERHEAD} more"
            ));
        }
    }
    bad
}

#[test]
fn every_job_has_a_limit_its_run_ends_inside() {
    let run = run_minutes();
    let found = unbounded(&workflow(".github/workflows/flake-check.yml"), run);
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn a_job_without_a_limit_or_too_short_for_its_run_is_found() {
    let wf = crate::harness::yaml(
        r#"
jobs:
  lane:
    steps: [{run: echo}]
  check:
    timeout-minutes: 200
    steps:
      - run: bash "$TOOLS/gate.sh"
      - {run: upload, timeout-minutes: 30}
  long:
    timeout-minutes: 300
    steps: [{run: echo}]
"#,
    );
    assert_eq!(
        unbounded(&wf, 180),
        [
            "lane: no timeout-minutes of its own",
            "check: 200 minutes, under its run's 180, its steps' 30 and 30 more",
            "long: 300 minutes, past 240",
        ]
    );
}
