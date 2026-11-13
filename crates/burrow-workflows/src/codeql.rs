//! The shared code scanning's `pick` step, its own script run with gh stood in
//! for: a PR scans the languages its files are in, a push scans them all, and
//! a language the step has no pattern for is never left out.

use serde_json::{Value as Json, json};

use crate::harness::{Ran, Scratch, script, step, workflow, write};

fn jobs() -> yaml_serde::Value {
    workflow(".github/workflows/codeql.yml")["jobs"].clone()
}

const BURROW: &str = r#"["actions", "c-cpp", "javascript-typescript", "python", "rust"]"#;

/// gh as the step calls it, `gh api --paginate PATH --jq EXPR`, answered
/// from a pulls/N/files response.
fn stand_in() -> Scratch {
    let s = Scratch::default();
    s.stub(
        "gh",
        r#"[ -z "$GH_FAIL" ] || exit 1
while [ $# -gt 0 ]; do
  [ "$1" = --jq ] && { exec jq -r "$2" "$FILES"; }
  shift
done
cat "$FILES"
"#,
    );
    s
}

fn run(event: &str, languages: &str, files: Json, fail: bool) -> Ran {
    let s = stand_in();
    let path = s.join("files.json");
    write(&path, &files.to_string());
    let jobs = jobs();
    s.bash(
        script(step(&jobs["pick"], "pick")),
        s.path(),
        &[
            ("FILES", path.to_str().unwrap()),
            ("GH_FAIL", if fail { "1" } else { "" }),
            ("GH_TOKEN", "t"),
            ("GITHUB_REPOSITORY", "sam/r"),
            ("EVENT", event),
            ("PR", "7"),
            ("LANGUAGES", languages),
        ],
    )
}

/// The languages the step picked, from what it wrote to GITHUB_OUTPUT.
fn picked(ran: &Ran) -> Vec<String> {
    assert_eq!(ran.code, 0, "{}", ran.said());
    let line = ran
        .outputs
        .lines()
        .find_map(|l| l.strip_prefix("languages="))
        .unwrap_or_else(|| panic!("no languages output: {}", ran.said()));
    serde_json::from_str(line).unwrap()
}

fn changed(names: &[&str]) -> Json {
    Json::Array(names.iter().map(|n| json!({"filename": n})).collect())
}

/// A push (the default branch's) scans every language, without asking GitHub
/// what changed.
#[test]
fn a_push_scans_every_language() {
    let ran = run("push", BURROW, json!("gh isn't called"), true);
    assert_eq!(
        picked(&ran),
        [
            "actions",
            "c-cpp",
            "javascript-typescript",
            "python",
            "rust"
        ]
    );
}

/// A PR scans only the languages of the files it changes.
#[test]
fn a_pr_scans_the_languages_it_touches() {
    let ran = run(
        "pull_request",
        BURROW,
        changed(&[
            "crates/burrow-proj/src/inbox/plans.rs",
            "docs/task-planning.md",
        ]),
        false,
    );
    assert_eq!(picked(&ran), ["rust"]);
    let ran = run(
        "pull_request",
        BURROW,
        changed(&[
            ".github/workflows/flake.yml",
            "modules/autoland/step.py",
            "Cargo.lock",
        ]),
        false,
    );
    assert_eq!(picked(&ran), ["actions", "python", "rust"]);
}

/// A PR that touches no scanned language's files scans nothing, and the
/// analyze job is skipped for it.
#[test]
fn a_docs_pr_scans_nothing() {
    let ran = run(
        "pull_request",
        BURROW,
        changed(&["README.md", "modules/proj.nix"]),
        false,
    );
    assert!(picked(&ran).is_empty());
    assert_eq!(
        jobs()["analyze"]["if"],
        "needs.pick.outputs.languages != '[]'"
    );
}

/// A rename counts its old path too: moving Python out of a file is a Python
/// change.
#[test]
fn a_rename_counts_its_old_path() {
    let ran = run(
        "pull_request",
        BURROW,
        json!([{"filename": "tools/sync.txt", "previous_filename": "tools/sync.py"}]),
        false,
    );
    assert_eq!(picked(&ran), ["python"]);
}

/// A language the step has no pattern for is scanned on every PR, never
/// dropped.
#[test]
fn a_language_without_a_pattern_is_always_scanned() {
    let ran = run(
        "pull_request",
        r#"["swift", "rust"]"#,
        changed(&["README.md"]),
        false,
    );
    assert_eq!(picked(&ran), ["swift"]);
}

/// When GitHub can't say what changed, the step fails rather than scan
/// nothing.
#[test]
fn a_failed_listing_fails_the_step() {
    let ran = run("pull_request", BURROW, changed(&["a.rs"]), true);
    assert_ne!(ran.code, 0, "{}", ran.said());
    assert!(!ran.outputs.contains("languages="), "{}", ran.outputs);
}
