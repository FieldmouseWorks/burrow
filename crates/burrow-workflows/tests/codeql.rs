//! The shared code scanning's wiring: `pick` builds crates/burrow-codeql
//! from the workflow's own commit and runs it with the event (its own tests
//! say what it picks), the analyze matrix is what it picked, and burrow's
//! caller reruns on a retarget and cancels a PR's last run whole.
//!
//! An integration test, where the crate's other workflows' tests are its
//! modules: Cargo finds this file by itself, so it builds wherever the crate
//! does, a tree without the workflows it reads included, rather than waiting
//! on a module declaration to take it in.

#[allow(dead_code)]
#[path = "../src/harness.rs"]
mod harness;

use harness::{script, step, text, workflow};

fn jobs() -> yaml_serde::Value {
    workflow(".github/workflows/codeql.yml")["jobs"].clone()
}

/// `pick` runs the picker of the workflow's own commit, whoever calls it,
/// built with the toolchain the crates pin, and hands it the event, the PR
/// and the caller's languages; its output is the analyze job's matrix.
#[test]
fn pick_runs_the_picker_of_the_workflows_own_commit() {
    let jobs = jobs();
    let pick = &jobs["pick"];
    let checkout = pick["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|s| {
            s["uses"]
                .as_str()
                .is_some_and(|u| u.starts_with("actions/checkout@"))
        })
        .expect("a checkout of the picker");
    assert_eq!(
        checkout["with"]["repository"],
        "${{ job.workflow_repository }}"
    );
    assert_eq!(checkout["with"]["ref"], "${{ job.workflow_sha }}");
    assert_eq!(checkout["with"]["sparse-checkout"], "crates");
    let path = text(&checkout["with"]["path"]);
    let run = step(pick, "pick");
    assert_eq!(run["working-directory"], format!("{path}/crates").as_str());
    let script = script(run);
    assert!(
        script.contains("cargo build --release --locked --quiet -p burrow-codeql"),
        "{script}"
    );
    assert!(
        script.contains(r#""$CARGO_TARGET_DIR/release/burrow-codeql" pick"#),
        "{script}"
    );
    let env = &run["env"];
    assert_eq!(env["EVENT"], "${{ github.event_name }}");
    assert_eq!(env["PR"], "${{ github.event.pull_request.number }}");
    assert_eq!(env["LANGUAGES"], "${{ inputs.languages }}");
    assert_eq!(env["GH_TOKEN"], "${{ github.token }}");
    assert_eq!(
        pick["outputs"]["languages"],
        "${{ steps.pick.outputs.languages }}"
    );
    assert_eq!(
        jobs["analyze"]["strategy"]["matrix"]["language"],
        "${{ fromJSON(needs.pick.outputs.languages) }}"
    );
}

/// A PR that touches no scanned language's files skips the analyze job.
#[test]
fn an_empty_pick_skips_analysis() {
    assert_eq!(
        jobs()["analyze"]["if"],
        "needs.pick.outputs.languages != '[]'"
    );
}

/// burrow's caller reruns on a retarget, not on a title or body edit, and a
/// PR's new run cancels its last one whole when it's made: the analysis jobs
/// have no group of their own that an older run could cancel a newer one by.
#[test]
fn the_caller_reruns_on_a_retarget_and_cancels_whole_runs() {
    let caller = workflow(".github/workflows/code-scanning.yml");
    let types = caller["on"]["pull_request"]["types"].as_sequence().unwrap();
    assert!(types.iter().any(|t| t == "edited"), "{types:?}");
    assert_eq!(
        caller["jobs"]["codeql"]["if"],
        "github.event.action != 'edited' || github.event.changes.base"
    );
    assert_eq!(caller["concurrency"]["cancel-in-progress"], true);
    let group = caller["concurrency"]["group"].as_str().unwrap();
    assert!(group.contains("github.event.changes.base"), "{group}");
    assert!(group.contains("format('pr-{0}', github.event.pull_request.number)"));
    assert!(group.contains("format('run-{0}', github.run_id)"));
    assert!(jobs()["analyze"].get("concurrency").is_none());
}
