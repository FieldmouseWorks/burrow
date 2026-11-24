//! The build store's shadow in the check job (docs/gate-design.md, stage
//! 1): with the caller's Actions variables naming a store, each leg times
//! its kept builds' restore, says before it builds what the store would
//! have served (the gate step: gate/store.rs, and the runner's builds.rs),
//! and once its gate ran, passed or failed, uploads what it built
//! (crates/burrow-keep's upload.rs) in a soft step of its own. With no
//! store named, none of it runs and every other step runs as it did. No
//! step names the store as a substituter: where a run substitutes from is
//! never what a caller says.

use std::collections::BTreeMap;

use yaml_serde::Value;

use crate::expr::{Ctx, V, value};
use crate::harness::{read, repo_file, script, step, steps, text, workflow};

fn job() -> Value {
    workflow(".github/workflows/flake-check.yml")["jobs"]["check"].clone()
}

/// The steps of the build store, by id or reporter's name.
fn ours(s: &Value) -> bool {
    s["id"].as_str().is_some_and(|i| i.starts_with("builds-"))
        || s["name"]
            .as_str()
            .is_some_and(|n| n.starts_with("degraded/builds-"))
}

fn label(s: &Value) -> String {
    s["id"]
        .as_str()
        .or_else(|| s["name"].as_str())
        .or_else(|| s["uses"].as_str())
        .or_else(|| s["run"].as_str())
        .unwrap_or("?")
        .to_owned()
}

/// The steps that run, as a runner works their `if`s out on `c`: each that
/// runs ends as `outcomes` says, or succeeds.
fn ran(job: &Value, c: &Ctx, outcomes: &BTreeMap<&str, &str>) -> Vec<String> {
    let mut c = c.clone();
    let mut out = Vec::new();
    for s in steps(job) {
        let runs = match &s["if"] {
            Value::Null => true,
            Value::String(cond) if cond.trim_start().starts_with("${{") => value(cond, &c).truthy(),
            Value::String(cond) => value(&format!("${{{{ {cond} }}}}"), &c).truthy(),
            other => panic!("{other:?}"),
        };
        if let Some(id) = s["id"].as_str() {
            let o = if runs {
                outcomes.get(id).copied().unwrap_or("success")
            } else {
                "skipped"
            };
            c.insert(format!("steps.{id}.outcome"), o.into());
        }
        if runs {
            out.push(label(s));
        }
    }
    out
}

/// Every output a step's `if` reads, empty: as when the step said nothing.
fn quiet(job: &Value, c: &mut Ctx) {
    for s in steps(job) {
        let t = serde_json::to_string(&s["if"]).unwrap();
        for part in t.split("steps.").skip(1) {
            let key: String = part
                .chars()
                .take_while(|ch| ch.is_ascii_alphanumeric() || "-_.".contains(*ch))
                .collect();
            if key.contains(".outputs.") {
                c.entry(format!("steps.{key}")).or_insert(V::Null);
            }
        }
    }
}

/// A run: its event, its ref and where a PR comes from, keep-builds and
/// install-nix, and the store variables.
fn run(event: &str, git_ref: &str, head: Option<&str>, keep: bool, nix: bool, url: &str) -> Ctx {
    let mut c: Ctx = [
        ("cancelled()", V::from(false)),
        ("always()", true.into()),
        ("inputs.keep-builds", keep.into()),
        ("inputs.install-nix", nix.into()),
        ("inputs.legs", V::from(1)),
        ("inputs.gate-reuse", false.into()),
        ("inputs.flaky", false.into()),
        ("github.event_name", event.into()),
        ("github.ref", git_ref.into()),
        ("github.repository", "owner/repo".into()),
        ("github.event.repository.default_branch", "main".into()),
        ("github.event.repository.private", false.into()),
        (
            "github.event.pull_request.head.repo.full_name",
            head.map_or(V::Null, V::from),
        ),
        ("needs.reuse.outputs.hit", V::Null),
        ("vars.BURROW_BUILDS_URL", url.into()),
        (
            "vars.BURROW_BUILDS_PUBLIC_KEY",
            V::from(if url.is_empty() { "" } else { "machine-1:K" }),
        ),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    quiet(&job(), &mut c);
    c
}

const MAIN: &str = "refs/heads/main";
const PR: &str = "refs/pull/7/merge";

/// Every kind of run: a push to the default branch or another, a PR from
/// the repo and one from a fork, with keep-builds and nix's install each on
/// and off.
fn runs(url: &str) -> Vec<(String, Ctx)> {
    let mut all = Vec::new();
    for (event, git_ref, head) in [
        ("push", MAIN, None),
        ("push", "refs/heads/conary/train/1", None),
        ("pull_request", PR, Some("owner/repo")),
        ("pull_request", PR, Some("someone/repo")),
    ] {
        for keep in [true, false] {
            for nix in [true, false] {
                all.push((
                    format!("{event} {git_ref} {head:?} keep={keep} nix={nix}"),
                    run(event, git_ref, head, keep, nix, url),
                ));
            }
        }
    }
    all
}

/// With no store named, none of its steps runs, the gate is told of none,
/// and every other step runs as it does in the job without them: passed
/// or failed, whatever the run.
#[test]
fn with_no_store_named_every_step_runs_as_it_did() {
    let job = job();
    let mut without = job.clone();
    without["steps"]
        .as_sequence_mut()
        .unwrap()
        .retain(|s| !ours(s));
    assert!(
        steps(&without).len() + 6 == steps(&job).len(),
        "the store's steps are six: the clock's two, the upload's two and their reporters"
    );
    for (case, c) in runs("") {
        for gate in ["success", "failure"] {
            let outcomes = BTreeMap::from([("gate", gate)]);
            let with = ran(&job, &c, &outcomes);
            assert_eq!(with, ran(&without, &c, &outcomes), "{case} {gate}");
            assert!(
                !with.iter().any(|l| l.starts_with("builds-")),
                "{case}: {with:?}"
            );
        }
        let env = &step(&job, "gate")["env"];
        for k in ["BUILDS", "BUILDS_KEY"] {
            assert_eq!(value(text(&env[k]), &c).text(), "", "{case} {k}");
        }
    }
}

/// With a store named, a leg on a runner whose store is its own uploads
/// once its gate ran, passed or failed, whatever the run (the door decides
/// who may write), after every step that keeps builds, from the tool the
/// keep step built where it built one; never once the gate didn't run
/// (gate-reuse's hit, a cancelled run), nor on a runner that shares the
/// machine's store, where "unsigned" isn't "this job's".
#[test]
fn a_leg_writes_only_after_its_gate_ran_on_a_store_of_its_own() {
    let job = job();
    let url = "https://builds.example/builds";
    for (case, c) in runs(url) {
        let nix = c["inputs.install-nix"] == V::from(true);
        for gate in ["success", "failure", "cancelled", "skipped"] {
            let ran_gate = gate == "success" || gate == "failure";
            for keep_tool in ["success", "failure"] {
                let outcomes = BTreeMap::from([("gate", gate), ("keep-tool", keep_tool)]);
                let did = ran(&job, &c, &outcomes);
                let wrote = did.iter().any(|l| l == "builds-write");
                assert_eq!(wrote, nix && ran_gate, "{case} {gate} {keep_tool}: {did:?}");
                // Its own checkout only where the keep step's didn't happen.
                let kept_tool = did.iter().any(|l| l == "keep-tool") && keep_tool == "success";
                assert_eq!(
                    did.iter().any(|l| l == "builds-tool"),
                    nix && ran_gate && !kept_tool,
                    "{case} {gate} {keep_tool}: {did:?}"
                );
            }
        }
        let mut cancelled = c.clone();
        cancelled.insert("cancelled()".into(), true.into());
        let did = ran(&job, &cancelled, &BTreeMap::new());
        assert!(!did.iter().any(|l| l == "builds-write"), "{case} cancelled");
    }
    // After the gate and every step of the kept builds.
    let all = steps(&job);
    let at = |id: &str| all.iter().position(|s| s["id"] == id).unwrap();
    for id in [
        "gate",
        "keep-tool",
        "keep",
        "keep-newer",
        "keep-save",
        "keep-prune",
    ] {
        assert!(
            at(id) < at("builds-tool") && at("builds-tool") < at("builds-write"),
            "{id}"
        );
    }
    let (tool, write) = (step(&job, "builds-tool"), step(&job, "builds-write"));
    let keep_tool = step(&job, "keep-tool");
    assert_eq!(tool["uses"], keep_tool["uses"]);
    assert_eq!(tool["with"], keep_tool["with"]);
    assert_eq!(
        write["working-directory"],
        step(&job, "keep")["working-directory"]
    );
    // Built where the keep step builds, so where both run it's built once.
    assert_eq!(
        write["env"]["CARGO_TARGET_DIR"],
        step(&job, "keep")["env"]["CARGO_TARGET_DIR"]
    );
    assert_eq!(write["env"]["BUILDS"], "${{ vars.BURROW_BUILDS_URL }}");
    assert!(
        write["continue-on-error"] == true
            && tool["continue-on-error"] == true
            && write["timeout-minutes"].as_u64().is_some_and(|m| m <= 60),
        "{write:?}"
    );
    assert_eq!(
        script(write),
        "cargo build --release --locked --quiet -p burrow-keep --bin burrow-keep-upload\n\
         \"$CARGO_TARGET_DIR/release/burrow-keep-upload\" \"$BUILDS\"\n"
    );
    assert!(repo_file("crates/burrow-keep/src/bin/burrow-keep-upload.rs").is_file());
}

/// The restore is timed from just before its lookup to just after it, on
/// the runner's clock, into the files the gate reads; only with a store
/// named, and its end only once the restore ran.
#[test]
fn the_kept_builds_restore_is_timed_for_the_report() {
    let job = job();
    let all = steps(&job);
    let at = |f: &dyn Fn(&Value) -> bool| all.iter().position(f).unwrap();
    let id = |i: &'static str| move |s: &Value| s["id"] == i;
    let install = at(&|s: &Value| {
        s["uses"]
            .as_str()
            .is_some_and(|u| u.starts_with("cachix/install-nix-action@"))
    });
    assert!(at(&id("builds-restore-start")) + 1 == at(&id("kept-lookup")));
    assert!(at(&id("kept-restore")) + 1 == at(&id("builds-restore-end")));
    assert!(at(&id("builds-restore-end")) < install);
    let runner = read(&repo_file("crates/burrow-flake-check/src/builds.rs"));
    for (i, file) in [
        ("builds-restore-start", "builds-restore-start"),
        ("builds-restore-end", "builds-restore-end"),
    ] {
        assert_eq!(
            script(step(&job, i)),
            format!("date +%s%N > \"$RUNNER_TEMP/{file}\"")
        );
        assert!(
            runner.contains(&format!("\"{file}\"")),
            "the gate doesn't read {file}"
        );
    }
    for (case, c) in runs("https://builds.example/builds") {
        let did = ran(&job, &c, &BTreeMap::new());
        let restored = did.iter().any(|l| l == "kept-restore");
        assert_eq!(
            did.iter().any(|l| l == "builds-restore-end"),
            restored,
            "{case}"
        );
        let mut skipped = BTreeMap::new();
        skipped.insert("kept-restore", "failure");
        let did = ran(&job, &c, &skipped);
        assert!(!did.iter().any(|l| l == "builds-restore-end"), "{case}");
    }
}

/// The store's variables reach only the steps that time, report and
/// upload, and the gate as its two names; none names it to nix as a
/// substituter, in any job.
#[test]
fn no_step_adds_the_store_as_a_substituter() {
    let wf = workflow(".github/workflows/flake-check.yml");
    let mut named = Vec::new();
    for (job, body) in wf["jobs"].as_mapping().unwrap() {
        if body["steps"].is_null() {
            continue;
        }
        for s in steps(body) {
            let all = serde_json::to_string(s).unwrap();
            assert!(
                !all.contains("vars.") || all.contains("vars.BURROW_BUILDS_"),
                "{job:?}: another variable: {all}"
            );
            if all.contains("vars.") {
                named.push(format!("{}/{}", text(job), label(s)));
            }
            for k in [
                "extra_nix_config",
                "nix_conf",
                "NIX_CONFIG",
                "NIX_USER_CONF_FILES",
            ] {
                let v = serde_json::to_string(&s["with"][k]).unwrap()
                    + &serde_json::to_string(&s["env"][k]).unwrap();
                assert!(
                    !v.contains("vars.") && !v.contains("BUILDS"),
                    "{job:?}: {v}"
                );
            }
        }
    }
    assert_eq!(
        named,
        [
            "check/builds-restore-start",
            "check/gate",
            "check/builds-tool",
            "check/builds-write",
        ]
    );
    // The gate's own: the URL and key, which it asks with `--store` alone.
    let check = job();
    let gate = step(&check, "gate");
    let env = serde_json::to_string(&gate["env"]).unwrap();
    assert_eq!(env.matches("vars.").count(), 2, "{env}");
    let runner = read(&repo_file("crates/burrow-flake-check/src/builds.rs"));
    assert!(
        !runner.contains("substituters"),
        "the runner names a substituter"
    );
}
