//! burrow's own caller and its merge train: a push to a `conary/train/`
//! branch, the candidate the train would land, runs the whole gate, its four
//! legs and all, in `train`, from main's copy of the shared workflow, the only
//! copy whose result Conary counts (its docs/ci-attestation.md, "Checking
//! one", rule 2); `check` stays off it, so a candidate takes one gate, not
//! two. A PR to main gets `train`'s negative run too, beside its own whole
//! gate in `check`, which stays until the train lands burrow's PRs. The
//! train asks for the check runs these name: `train / check` and
//! `train / negative`.

use yaml_serde::Value;

use crate::expr::{Ctx, V, condition, value};
use crate::harness::{Scratch, script, step, workflow, yaml};

const CALLER: &str = ".github/workflows/flake.yml";
const RETARGET: &str = ".github/workflows/flake-retarget.yml";
/// The check runs the train's configuration names (remi's modules/burrow.nix,
/// `trainRequire` and `trainNegative`).
const GATE: &str = "train / check";
const NEGATIVE: &str = "train / negative";
/// The shared workflow as main has it.
const MAINS: &str = "FieldmouseWorks/burrow/.github/workflows/flake-check.yml@main";

/// A run of the caller: its event, the pushed ref or the PR's base.
struct Run {
    event: &'static str,
    git_ref: &'static str,
    base: &'static str,
}

impl Run {
    fn ctx(&self) -> Ctx {
        let pr = self.event == "pull_request";
        [
            ("github.event_name", V::from(self.event)),
            ("github.ref", V::from(self.git_ref)),
            (
                "github.event.pull_request.base.ref",
                if pr { V::from(self.base) } else { V::Null },
            ),
            ("github.event.repository.default_branch", V::from("main")),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect()
    }
}

/// Whether the caller's job runs: no `if` runs it on every run, and YAML's
/// own true or false says so whatever the run.
fn runs(job: &Value, ctx: &Ctx) -> bool {
    match job.get("if") {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(Value::String(_)) => condition(text_if(job), ctx),
        Some(other) => panic!("an if that's neither text nor a boolean: {other:?}"),
    }
}

/// Execute the caller's router with the workflow's own environment bindings,
/// then evaluate the dependent call's condition using its actual output.
fn routes(wf: &Value, run: &Run, from: Option<&str>) -> (bool, bool) {
    let mut ctx = run.ctx();
    ctx.insert(
        "github.event.changes.base".into(),
        from.map_or(V::Null, |base| {
            format!("{{ref: {{from: {base}}}}}").as_str().into()
        }),
    );
    let router = &wf["jobs"]["train-route"];
    assert_eq!(router["runs-on"], "ubuntu-latest");
    assert_eq!(router["permissions"], yaml("{}"));
    assert_eq!(
        router["outputs"]["train"],
        "${{ steps.route.outputs.train }}"
    );
    assert_eq!(wf["jobs"]["train"]["needs"], "train-route");
    let output = if runs(router, &ctx) {
        let s = Scratch::default();
        let route = step(router, "route");
        let env: Vec<_> = route["env"]
            .as_mapping()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.as_str().unwrap(), value(v.as_str().unwrap(), &ctx).text()))
            .collect();
        let env: Vec<_> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let ran = s.bash(script(route), s.path(), &env);
        assert_eq!(ran.code, 0, "{}", ran.said());
        ran.outputs
            .trim()
            .strip_prefix("train=")
            .unwrap()
            .to_owned()
    } else {
        String::new()
    };
    ctx.insert(
        "needs.train-route.outputs.train".into(),
        output.as_str().into(),
    );
    (
        runs(&wf["jobs"]["check"], &ctx),
        runs(&wf["jobs"]["train"], &ctx),
    )
}

/// Which of the caller's two calls each run takes.
#[test]
fn a_candidate_takes_the_train_gate_and_only_it() {
    let wf = workflow(CALLER);
    // A candidate's push starts a run at all.
    assert_eq!(
        wf["on"]["push"]["branches"],
        yaml(r#"[main, "conary/train/**"]"#),
        "{CALLER} doesn't run on the train's candidates"
    );
    for (run, takes_check, takes_train) in [
        // main's own push: its gate keeps the builds later runs take.
        (
            Run {
                event: "push",
                git_ref: "refs/heads/main",
                base: "",
            },
            true,
            false,
        ),
        // A candidate: main with PRs on top, as Conary names its branches
        // (remi's train has pushed conary/train/268tq1gyyzm3yg3pnkj745s281).
        (
            Run {
                event: "push",
                git_ref: "refs/heads/conary/train/268tq1gyyzm3yg3pnkj745s281",
                base: "",
            },
            false,
            true,
        ),
        (
            Run {
                event: "push",
                git_ref: "refs/heads/conary/train/a/b",
                base: "",
            },
            false,
            true,
        ),
        // A PR to main: its whole gate, and the train's negative.
        (
            Run {
                event: "pull_request",
                git_ref: "refs/pull/9/merge",
                base: "main",
            },
            true,
            true,
        ),
        // A PR to another branch, which the train never lands: its gate only.
        (
            Run {
                event: "pull_request",
                git_ref: "refs/pull/9/merge",
                base: "release",
            },
            true,
            false,
        ),
        // Distinct branches even though GitHub's expression equality ignores case.
        (
            Run {
                event: "pull_request",
                git_ref: "refs/pull/9/merge",
                base: "Main",
            },
            true,
            false,
        ),
        (
            Run {
                event: "pull_request",
                git_ref: "refs/pull/9/merge",
                base: "MAIN",
            },
            true,
            false,
        ),
        // A branch under conary/ that isn't the train's.
        (
            Run {
                event: "push",
                git_ref: "refs/heads/conary/trains",
                base: "",
            },
            true,
            false,
        ),
    ] {
        let at = format!("{} {} {}", run.event, run.git_ref, run.base);
        assert_eq!(routes(&wf, &run, None), (takes_check, takes_train), "{at}");
    }
}

/// Retargeting in either direction uses the new base, never the old one.
/// Edits that leave the base alone start neither gate nor router.
#[test]
fn retargets_and_case_variants_keep_only_their_intended_work() {
    let (caller, retarget) = (workflow(CALLER), workflow(RETARGET));
    let mut router = retarget["jobs"]["train-route"].clone();
    assert_eq!(router["if"], "github.event.changes.base");
    router.as_mapping_mut().unwrap().remove("if");
    assert_eq!(router, caller["jobs"]["train-route"]);
    for base in ["main", "Main", "MAIN", "release", "main2"] {
        let run = Run {
            event: "pull_request",
            git_ref: "refs/pull/9/merge",
            base,
        };
        let expected = (true, base == "main");
        assert_eq!(routes(&caller, &run, None), expected, "{base}");
        for from in ["main", "Main", "MAIN", "release"] {
            assert_eq!(
                routes(&retarget, &run, Some(from)),
                expected,
                "{from} -> {base}"
            );
        }
        assert_eq!(
            routes(&retarget, &run, None),
            (false, false),
            "edit, {base}"
        );

        // With the train selected, the shared workflow must give an exact
        // default PR only negative/commit work. The check call keeps its
        // existing gate, including on case variants and other bases.
        let shared = workflow(".github/workflows/flake-check.yml");
        let s = Scratch::default();
        for name in ["check", "train"] {
            if name == "train" && !expected.1 {
                continue;
            }
            let pr_gate = caller["jobs"][name]["with"]["pr-gate"]
                .as_bool()
                .unwrap_or(true);
            let ran = s.bash(
                script(step(&shared["jobs"]["lane"], "base")),
                s.path(),
                &[
                    ("BASE", base),
                    ("DEFAULT", "main"),
                    ("PR_GATE", if pr_gate { "true" } else { "false" }),
                ],
            );
            assert_eq!(ran.code, 0, "{}", ran.said());
            let lands = ran.outputs.trim() == "lands=true";
            assert_eq!(lands, name == "train", "{name}, {base}");
            let mut ctx = run.ctx();
            ctx.extend([
                ("cancelled()".into(), false.into()),
                ("needs.lane.result".into(), "success".into()),
                (
                    "needs.lane.outputs.gate".into(),
                    if lands { "train" } else { "full" }.into(),
                ),
            ]);
            assert_eq!(
                runs(&shared["jobs"]["check"], &ctx),
                !lands,
                "{name}, {base}"
            );
            for work in ["negative", "commits"] {
                assert!(runs(&shared["jobs"][work], &ctx), "{name}, {base}, {work}");
            }
        }
    }
}

/// `train` is `check`'s gate, from main's copy, with the PR's own gate left to
/// `check`: the same inputs, pr-gate off.
#[test]
fn the_train_gate_is_the_whole_gate_from_mains_copy() {
    let wf = workflow(CALLER);
    let (check, train) = (&wf["jobs"]["check"], &wf["jobs"]["train"]);
    // Signed by the workflow at a commit on main, which is all Conary counts:
    // a candidate's own copy (`./`) is a branch's.
    assert_eq!(train["uses"], MAINS);
    assert_eq!(check["uses"], "./.github/workflows/flake-check.yml");
    assert_eq!(train["permissions"], check["permissions"]);
    assert_eq!(train["permissions"]["id-token"], "write");
    let mut with = check["with"].clone();
    with.as_mapping_mut()
        .unwrap()
        .insert("pr-gate".into(), false.into());
    assert_eq!(train["with"], with);
    // The four legs, and the agents check skipped only where it always was.
    assert_eq!(train["with"]["legs"], 4);
    assert_eq!(train["with"]["build-skip"], "agents");
}

/// The names the train's configuration gives: the gate's own job, which with
/// legs is the one named check, and the negative's, each under `train`; the
/// gate runs and signs on a push, the negative on a PR.
#[test]
fn the_train_asks_for_names_the_shared_workflow_gives() {
    let shared = workflow(".github/workflows/flake-check.yml");
    let train = &workflow(CALLER)["jobs"]["train"];
    let legs = train["with"]["legs"].as_u64().expect("legs");
    let mut ctx: Ctx = [
        ("inputs.legs", V::from(legs)),
        ("github.event_name", V::from("push")),
        ("cancelled()", V::from(false)),
        ("needs.lane.result", V::from("skipped")),
        ("needs.lane.outputs.gate", V::Null),
        ("needs.check.result", V::from("success")),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let jobs = &shared["jobs"];
    let gate = &jobs[if legs > 1 { "gate" } else { "check" }];
    let name = value(gate["name"].as_str().expect("a name"), &ctx).text();
    assert_eq!(format!("train / {name}"), GATE);
    // On a candidate's push: its legs, then the gate job, which signs.
    assert!(
        condition(text_if(&jobs["check"]), &ctx),
        "the legs skip a push"
    );
    assert!(condition(text_if(gate), &ctx), "the gate job skips a push");
    let subject = gate["steps"]
        .as_sequence()
        .unwrap()
        .iter()
        .find(|s| s["id"] == "subject")
        .expect("a subject step");
    ctx.insert("steps.gate.outcome".into(), V::from("success"));
    assert!(
        condition(text_if(subject), &ctx),
        "a push's gate isn't signed"
    );

    // Its check run is named for its job, which has no name of its own.
    let negative = &jobs["negative"];
    assert!(negative.get("name").is_none(), "{:?}", negative["name"]);
    assert_eq!(NEGATIVE.strip_prefix("train / "), Some("negative"));
    ctx.insert("github.event_name".into(), V::from("pull_request"));
    assert!(
        condition(text_if(negative), &ctx),
        "a PR has no negative run"
    );
}

fn text_if(job: &Value) -> &str {
    let c = job["if"].as_str().expect("an if");
    c.strip_prefix("${{")
        .and_then(|c| c.strip_suffix("}}"))
        .unwrap_or(c)
}
