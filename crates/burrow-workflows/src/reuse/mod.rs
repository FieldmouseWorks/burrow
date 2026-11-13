//! gate-reuse: off unless a caller turns it on, and then only a push to the
//! default branch asks its lookup, before anything is built, whether CI
//! attested a pass of this same gate on this exact tree. A hit, and only a
//! hit the runner read whole, for this request, naming one of this
//! repository's runs, builds, attests and keeps nothing: the job named check
//! (the gate job, with legs) reports the pass and links that run. A miss,
//! an error, an answer it can't read or one that can't tell runs the whole
//! gate, keeping its builds as it always has. The gate a run is (its
//! workflow's repository, file, commit and the commit's tree, and the
//! inputs that can change a result) is what every attestation names and
//! what the lookup is asked for, from one step.
//!
//! The runner's own reading of answers has unit tests beside it
//! (crates/burrow-flake-check/src/reuse.rs); these run the workflow's steps,
//! the lookup's with the real runner and a stand-in for the lookup that
//! keeps the contract (crates/burrow-flake-check/README.md).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use regex::Regex;
use yaml_serde::Value;

use crate::expr::{Ctx, V, value};
use crate::harness::{Scratch, read, script, step, steps, text, workflow, write};

mod lookup;

const WF: &str = ".github/workflows/flake-check.yml";
const CONTRACT: &str = "burrow-gate-reuse/v1";
const RUN: &str = "https://github.com/owner/repo/actions/runs/41";

fn wf() -> Value {
    workflow(WF)
}

fn ctx(pairs: &[(&str, V)]) -> Ctx {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// Whether `cond` (an `if:`, with or without `${{ }}`) holds.
fn holds(cond: &str, c: &Ctx) -> bool {
    let t = cond.trim();
    if t.starts_with("${{") {
        value(t, c).truthy()
    } else {
        value(&format!("${{{{ {t} }}}}"), c).truthy()
    }
}

fn job_runs(job: &Value, c: &Ctx) -> bool {
    holds(text(&job["if"]), c)
}

/// What a step is called in what these say: its id, its name, or the action
/// it uses.
fn label(s: &Value) -> String {
    s["id"]
        .as_str()
        .or_else(|| s["name"].as_str())
        .or_else(|| s["uses"].as_str().and_then(|u| u.split('@').next()))
        .or_else(|| s["run"].as_str())
        .unwrap_or("?")
        .to_owned()
}

/// The steps of `job` that run, in order, as a runner works their `if`s out
/// on `c`: each one that runs succeeds, and every step output is empty.
fn ran(job: &Value, c: &Ctx) -> Vec<String> {
    let mut c = c.clone();
    let refs = Regex::new(r"steps\.[A-Za-z0-9_-]+\.outputs\.[A-Za-z0-9_-]+").unwrap();
    for s in steps(job) {
        if let Some(cond) = s["if"].as_str() {
            for m in refs.find_iter(cond) {
                c.entry(m.as_str().to_owned()).or_insert(V::Null);
            }
        }
    }
    let mut out = Vec::new();
    for s in steps(job) {
        let runs = match &s["if"] {
            Value::Null => true,
            Value::Bool(b) => *b,
            Value::String(cond) => holds(cond, &c),
            other => panic!("an if that's neither text nor a boolean: {other:?}"),
        };
        if let Some(id) = s["id"].as_str() {
            let outcome = if runs { "success" } else { "skipped" };
            c.insert(format!("steps.{id}.outcome"), outcome.into());
        }
        if runs {
            out.push(label(s));
        }
    }
    out
}

/// A push to `git_ref`, with the lookup's hit as `hit` says (null: no
/// answer, gate-reuse off or the reuse job skipped or failed).
fn push(git_ref: &str, legs: u64, reuse: bool, hit: V) -> Ctx {
    ctx(&[
        ("cancelled()", false.into()),
        ("always()", true.into()),
        ("inputs.keep-builds", true.into()),
        ("inputs.install-nix", true.into()),
        ("inputs.legs", legs.into()),
        ("inputs.gate-reuse", reuse.into()),
        ("github.event_name", "push".into()),
        ("github.ref", git_ref.into()),
        ("github.event.repository.default_branch", "main".into()),
        ("github.event.repository.private", false.into()),
        ("github.repository", "owner/repo".into()),
        ("github.event.pull_request.head.repo.full_name", V::Null),
        ("needs.lane.result", "skipped".into()),
        ("needs.lane.outputs.gate", V::Null),
        ("needs.reuse.outputs.hit", hit),
    ])
}

const MAIN: &str = "refs/heads/main";

/// Off unless asked, and no caller burrow ships turns it on; then a push to
/// the default branch asks, and nothing else does: not a train's candidate
/// (whose attestation the train counts), not a PR, not a manual run.
#[test]
fn off_by_default_and_only_a_default_branch_push_asks() {
    let wf = wf();
    let inputs = &wf["on"]["workflow_call"]["inputs"];
    assert_eq!(inputs["gate-reuse"]["type"], "boolean");
    assert_eq!(inputs["gate-reuse"]["default"], false);
    assert_eq!(inputs["gate-reuse-lookup"]["type"], "string");
    assert_eq!(inputs["gate-reuse-lookup"]["default"], "");
    for caller in [
        ".github/workflows/flake.yml",
        ".github/workflows/flake-retarget.yml",
        "templates/project/.github/workflows/flake.yml",
        "templates/project/.github/workflows/flake-retarget.yml",
    ] {
        let jobs = workflow(caller)["jobs"].clone();
        for (name, job) in jobs.as_mapping().unwrap() {
            assert!(
                job["with"].get("gate-reuse").is_none()
                    && job["with"].get("gate-reuse-lookup").is_none(),
                "{caller}: {name:?} turns gate-reuse on"
            );
        }
    }
    let reuse = &wf["jobs"]["reuse"];
    for (event, git_ref, on, asks) in [
        ("push", MAIN, true, true),
        ("push", MAIN, false, false),
        ("push", "refs/heads/conary/train/7", true, false),
        ("push", "refs/heads/feature", true, false),
        ("pull_request", "refs/pull/12/merge", true, false),
        ("workflow_dispatch", MAIN, true, false),
    ] {
        let mut c = push(git_ref, 1, on, V::Null);
        c.insert("github.event_name".into(), event.into());
        assert_eq!(
            job_runs(reuse, &c),
            asks,
            "{event} {git_ref} gate-reuse {on}"
        );
    }
    // It reads nothing of the caller's repo, and can sign its request.
    assert_eq!(
        reuse["permissions"],
        crate::harness::yaml("{contents: read, id-token: write}")
    );
    assert_eq!(reuse["outputs"]["hit"], "${{ steps.lookup.outputs.hit }}");
    assert_eq!(reuse["outputs"]["run"], "${{ steps.lookup.outputs.run }}");
    // Its failure is soft, and says so where the API shows it.
    let lookup = step(reuse, "lookup");
    assert_eq!(lookup["continue-on-error"], true);
    assert_eq!(lookup["name"], "soft/gate-reuse");
    assert_eq!(script(lookup), "bash \"$TOOLS/run.sh\" reuse");
}

/// Off (the default), every run's steps are what they were: the lookup's
/// job is skipped and leaves no hit, so the gate builds, attests and keeps
/// as before, names no gate in what it attests, and nothing reports a
/// reuse.
#[test]
fn off_nothing_changes() {
    let wf = wf();
    let (check, gate) = (&wf["jobs"]["check"], &wf["jobs"]["gate"]);
    for legs in [1, 4] {
        let c = push(MAIN, legs, false, V::Null);
        assert!(job_runs(check, &c), "{legs} legs");
        let steps = ran(check, &c);
        for want in [
            "actions/checkout",
            "kept-lookup",
            "kept-restore",
            "cachix/install-nix-action",
            "tools",
            "gate",
            "keep-tool",
            "keep",
            "keep-save",
        ] {
            assert!(
                steps.iter().any(|s| s == want),
                "{legs} legs: {want}: {steps:?}"
            );
        }
        for never in ["identity", "reused"] {
            assert!(
                !steps.iter().any(|s| s == never),
                "{legs} legs: {never}: {steps:?}"
            );
        }
        if legs > 1 {
            let mut c = c.clone();
            c.insert("needs.check.result".into(), "success".into());
            assert!(job_runs(gate, &c));
            let steps = ran(gate, &c);
            assert!(steps.iter().any(|s| s == "gate"), "{steps:?}");
            assert!(
                !steps.iter().any(|s| s == "identity" || s == "reused"),
                "{steps:?}"
            );
        }
    }
}

/// A hit, with one leg: the job named check runs, and of its steps only the
/// one that reports the reuse does. Nothing is checked out, no Nix is
/// installed, no kept build is restored, the gate doesn't build, and
/// nothing is attested or kept.
#[test]
fn a_hit_with_one_leg_builds_nothing() {
    let wf = wf();
    let c = push(MAIN, 1, true, "true".into());
    assert!(job_runs(&wf["jobs"]["check"], &c));
    assert_eq!(ran(&wf["jobs"]["check"], &c), ["reused"]);
    let mut c = c;
    c.insert("needs.check.result".into(), "success".into());
    assert!(
        !job_runs(&wf["jobs"]["gate"], &c),
        "the gate job runs with one leg"
    );
}

/// A hit, with legs: no leg runs, and the gate job, named check, runs only
/// the step that reports the reuse, with the run it names.
#[test]
fn a_hit_with_legs_runs_no_leg() {
    let wf = wf();
    let (check, gate) = (&wf["jobs"]["check"], &wf["jobs"]["gate"]);
    for legs in [2, 4, 8] {
        let mut c = push(MAIN, legs, true, "true".into());
        assert!(!job_runs(check, &c), "{legs} legs: a leg runs");
        c.insert("needs.check.result".into(), "skipped".into());
        assert!(job_runs(gate, &c), "{legs} legs");
        assert_eq!(value(text(&gate["name"]), &c).text(), "check");
        assert_eq!(ran(gate, &c), ["reused"], "{legs} legs");
    }
    // The step that reports it is one step, in both jobs, linking the run.
    let reused = step(check, "reused");
    assert_eq!(reused, step(gate, "reused"));
    assert_eq!(reused["env"]["RUN"], "${{ needs.reuse.outputs.run }}");
    let s = Scratch::default();
    let summary = s.join("summary");
    let r = s.bash(
        script(reused),
        s.path(),
        &[
            ("RUN", RUN),
            ("GITHUB_STEP_SUMMARY", summary.to_str().unwrap()),
        ],
    );
    assert_eq!(r.code, 0, "{}", r.said());
    assert!(
        r.said().contains(RUN) && read(&summary).contains(RUN),
        "{}",
        r.said()
    );
}

/// No hit (a miss, or no answer at all): the whole gate, as without
/// gate-reuse, which also names the gate in what it attests; and only the
/// run that builds keeps what it built: a push to the default branch that
/// missed, not one that hit, nor a PR's, nor a train's candidate's, whose
/// entries only their own ref could restore.
#[test]
fn no_hit_runs_the_whole_gate_and_the_building_run_keeps() {
    let wf = wf();
    let (check, gate) = (&wf["jobs"]["check"], &wf["jobs"]["gate"]);
    for hit in [V::from("false"), V::from(""), V::Null] {
        for legs in [1, 4] {
            let c = push(MAIN, legs, true, hit.clone());
            assert!(job_runs(check, &c), "{hit:?} {legs}");
            let steps = ran(check, &c);
            for want in [
                "actions/checkout",
                "kept-restore",
                "cachix/install-nix-action",
                "tools",
                "identity",
                "gate",
                "keep-save",
            ] {
                assert!(
                    steps.iter().any(|s| s == want),
                    "{hit:?} {legs}: {want}: {steps:?}"
                );
            }
            assert!(!steps.iter().any(|s| s == "reused"), "{steps:?}");
            if legs == 1 {
                assert!(steps.iter().any(|s| s == "subject"), "{steps:?}");
            } else {
                let mut c = c.clone();
                c.insert("needs.check.result".into(), "success".into());
                let steps = ran(gate, &c);
                for want in ["tools", "identity", "gate", "subject"] {
                    assert!(steps.iter().any(|s| s == want), "{want}: {steps:?}");
                }
                assert!(!steps.iter().any(|s| s == "reused"), "{steps:?}");
            }
        }
    }
    let keeps = |c: &Ctx| ran(check, c).iter().any(|s| s == "keep-save");
    let restores = |c: &Ctx| ran(check, c).iter().any(|s| s == "kept-restore");
    assert!(keeps(&push(MAIN, 1, true, "false".into())));
    assert!(!keeps(&push(MAIN, 1, true, "true".into())));
    assert!(!restores(&push(MAIN, 1, true, "true".into())));
    assert!(!keeps(&push("refs/heads/conary/train/7", 1, true, V::Null)));
    assert!(restores(&push(
        "refs/heads/conary/train/7",
        1,
        true,
        V::Null
    )));
    let mut pr = push("refs/pull/12/merge", 1, true, V::Null);
    pr.insert("github.event_name".into(), "pull_request".into());
    pr.insert(
        "github.event.pull_request.head.repo.full_name".into(),
        "owner/repo".into(),
    );
    pr.insert("steps.kept-restore.outputs.cache-hit".into(), "true".into());
    assert!(!keeps(&pr));
    assert!(restores(&pr));
}

/// The identity step's environment, as the workflow gives it.
fn identity_env<'a>(over: &[(&'a str, &'a str)], sha: &'a str) -> Vec<(&'a str, &'a str)> {
    let mut env: BTreeMap<&str, &str> = [
        ("WORKFLOW_REPOSITORY", "FieldmouseWorks/burrow"),
        ("WORKFLOW_FILE", ".github/workflows/flake-check.yml"),
        ("WORKFLOW_SHA", sha),
        ("RUNNER_LABEL", "ubuntu-latest"),
        ("INSTALL_NIX", "true"),
        ("APART", "true"),
        ("LEGS", "4"),
        ("SKIP", "agents"),
        ("KEEP", "true"),
    ]
    .into_iter()
    .collect();
    for (k, v) in over {
        assert!(env.insert(k, v).is_some(), "no {k}");
    }
    env.into_iter().collect()
}

/// The workflow's own checkout, as `.flake-check` beside the step: a commit
/// of its own.
fn workflow_repo(s: &Scratch, at: &Path, text: &str) -> String {
    let repo = at.join(".flake-check");
    if !repo.exists() {
        fs::create_dir_all(&repo).unwrap();
        s.git(&repo, &["init", "-q"]);
    }
    s.commit(&repo, "flake-check.yml", text)
}

/// Runs the identity step in `cwd` (which holds `.flake-check`): its exit
/// code and what it wrote.
fn identity(s: &Scratch, cwd: &Path, env: &[(&str, &str)]) -> (i32, String, Option<String>) {
    let wf = wf();
    let temp = s.mkdtemp();
    let mut env = env.to_vec();
    env.push(("RUNNER_TEMP", temp.to_str().unwrap()));
    let r = s.bash(script(step(&wf["jobs"]["reuse"], "identity")), cwd, &env);
    let gate = temp.join("gate.json");
    (r.code, r.said(), gate.exists().then(|| read(&gate)))
}

/// The gate a run is: the workflow by its repository, file, commit and the
/// commit's tree, and the inputs that can change a result, build-skip as a
/// set; one sorted line. The step is one step, in the reuse job and in each
/// job that attests, and only runs there with gate-reuse on and the
/// workflow's files checked out.
#[test]
fn the_identity_is_the_workflows_commit_and_the_inputs_that_change_a_result() {
    let wf = wf();
    let id = step(&wf["jobs"]["reuse"], "identity");
    assert_eq!(id, step(&wf["jobs"]["check"], "identity"));
    assert_eq!(id, step(&wf["jobs"]["gate"], "identity"));
    assert_eq!(
        id["if"],
        "${{ inputs.gate-reuse && steps.tools.outcome == 'success' }}"
    );
    for job in ["reuse", "check", "gate"] {
        let tools = step(&wf["jobs"][job], "tools");
        assert_eq!(tools["with"]["path"], ".flake-check", "{job}");
        assert_eq!(tools["with"]["ref"], "${{ job.workflow_sha }}", "{job}");
        assert_eq!(
            tools["with"]["repository"], "${{ job.workflow_repository }}",
            "{job}"
        );
    }
    let s = Scratch::default();
    let at = s.mkdtemp();
    let sha = workflow_repo(&s, &at, "v1");
    let tree = s.git(&at.join(".flake-check"), &["rev-parse", "HEAD^{tree}"]);
    let (code, said, gate) = identity(&s, &at, &identity_env(&[("SKIP", " b  agents a b ")], &sha));
    assert_eq!(code, 0, "{said}");
    assert_eq!(
        gate.as_deref().unwrap(),
        format!(
            "{{\"inputs\":{{\"build-skip\":[\"a\",\"agents\",\"b\"],\"eval-apart\":true,\"install-nix\":true,\"keep-builds\":true,\"legs\":4,\"runner\":\"ubuntu-latest\"}},\"source\":\"tree\",\
             \"workflow\":{{\"commit\":\"{sha}\",\"file\":\".github/workflows/flake-check.yml\",\"repository\":\"FieldmouseWorks/burrow\",\"tree\":\"{tree}\"}}}}\n"
        )
    );
    // Not the workflow's commit: nothing.
    let (code, said, gate) = identity(&s, &at, &identity_env(&[], &"0".repeat(40)));
    assert_ne!(code, 0, "{said}");
    assert!(said.contains("not 0000"), "{said}");
    let _ = gate;
}

/// With gate-reuse on, what a gate attests names the gate it was, as the
/// identity step wrote it, after everything it said before; off, the
/// predicate is what it always was (attestation.rs holds it to that).
#[test]
fn the_attestation_names_the_gate() {
    let wf = wf();
    let subject = step(&wf["jobs"]["check"], "subject");
    let s = Scratch::default();
    s.stub(
        "curl",
        "echo '{\"count\": 1, \"value\": \"eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJ4In0.c2ln\"}'\n",
    );
    let repo = s.join("repo");
    fs::create_dir(&repo).unwrap();
    s.git(&repo, &["init", "-q"]);
    s.commit(&repo, "a", "a");
    let sha = s.git(&repo, &["rev-parse", "HEAD"]);
    let gate = "{\"inputs\":{\"legs\":1},\"workflow\":{\"commit\":\"c\"}}";
    let mut predicates = Vec::new();
    for with in [true, false] {
        let temp = s.mkdtemp();
        write(&temp.join("flake.log"), "checks passed\n");
        if with {
            write(&temp.join("gate.json"), &format!("{gate}\n"));
        }
        let r = s.bash(
            script(subject),
            &repo,
            &[
                ("RUNNER_TEMP", temp.to_str().unwrap()),
                ("GITHUB_EVENT_NAME", "push"),
                ("GITHUB_SHA", &sha),
                ("GITHUB_REPOSITORY", "owner/repo"),
                ("OUTCOME", "success"),
                ("SKIP", ""),
                ("PR_HEAD", ""),
                (
                    "ACTIONS_ID_TOKEN_REQUEST_URL",
                    "https://token.example/x?a=1",
                ),
                ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "t"),
            ],
        );
        assert_eq!(r.code, 0, "{}", r.said());
        let attested = read(&temp.join("conary/conary-attestation"));
        let line = attested.lines().nth(2).unwrap().to_owned();
        predicates.push(line);
    }
    // Last, so what came before it reads as it did.
    assert!(
        predicates[0].ends_with(&format!(",\"gate\":{gate}}}")),
        "{}",
        predicates[0]
    );
    let before = |p: &str| p.split(",\"log\":").next().unwrap().to_owned();
    assert_eq!(before(&predicates[0]), before(&predicates[1]));
    assert!(!predicates[1].contains("gate"), "{}", predicates[1]);
}
