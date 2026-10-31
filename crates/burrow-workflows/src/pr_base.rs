//! The shared CI's merge step, its own script run on real repos: a PR is
//! checked merged with its base branch as that branch is when the job
//! starts, not as it was when the event fired (a run that waited in the
//! queue, or a re-run), the log names both commits, and a head that no
//! longer merges cleanly fails the job without anything checked. With legs,
//! every leg, and the gate job after them, merges the head with the base as
//! the lane job found it instead, dated by its parents, so all of them check
//! one commit. The docs lane checks out and merges the same way, with the
//! same steps. Which lane a PR takes, the train's, the docs lane or the whole
//! gate, is lane's; here, the whole gate and the docs lane run on what the
//! lane job says, a lane job that failed giving the whole gate, and not at
//! all once the lane job was cancelled; with legs, the gate job once they've
//! all passed or failed. A PR's newer run cancels what its last one still
//! has going, and nothing else: not a push's run, another PR's, a sibling
//! call with inputs of its own, another of its legs, or (from a title or body
//! edit, which a retarget isn't) the same head's run.

use std::path::{Path, PathBuf};

use yaml_serde::Value;

use crate::expr::{Ctx, V, value};
use crate::harness::{Ran, Scratch, script, steps, text, workflow, yaml};

fn jobs() -> Value {
    workflow(".github/workflows/flake-check.yml")["jobs"].clone()
}

fn ctx(pairs: impl IntoIterator<Item = (&'static str, V)>) -> Ctx {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// Whether the whole gate and the docs lane run: the whole gate for a push,
/// and for a PR the lane job doesn't send to the train or the docs lane, a
/// lane job that failed included (fail closed); the docs lane only where the
/// lane job sent it; neither once the lane job was cancelled (a newer run of
/// the PR has it), nor once the run itself was.
#[test]
fn the_gate_runs_where_it_should() {
    let jobs = jobs();
    for (event, lane_result, lane_gate, cancelled, whole, docs_lane) in [
        ("push", "skipped", V::Null, false, true, false),
        ("pull_request", "success", "full".into(), false, true, false),
        (
            "pull_request",
            "success",
            "train".into(),
            false,
            false,
            false,
        ),
        ("pull_request", "success", "docs".into(), false, false, true),
        ("pull_request", "failure", V::Null, false, true, false),
        ("pull_request", "cancelled", V::Null, false, false, false),
        ("pull_request", "success", "full".into(), true, false, false),
        ("pull_request", "success", "docs".into(), true, false, false),
    ] {
        let c = ctx([
            ("cancelled()", cancelled.into()),
            ("github.event_name", event.into()),
            ("needs.lane.result", lane_result.into()),
            ("needs.lane.outputs.gate", lane_gate.clone()),
        ]);
        let case = format!("{event} {lane_result} {lane_gate:?} cancelled {cancelled}");
        assert_eq!(
            value(text(&jobs["check"]["if"]), &c).truthy(),
            whole,
            "{case}"
        );
        assert_eq!(
            value(text(&jobs["docs"]["if"]), &c).truthy(),
            docs_lane,
            "{case}"
        );
    }
    // The gate job: with legs, once they've all passed or failed; never with
    // one leg, nor once a leg was cancelled or none ran, nor once the run was.
    for (legs, check, cancelled, verdict) in [
        (4, "success", false, true),
        (4, "failure", false, true),
        (4, "cancelled", false, false),
        (4, "skipped", false, false),
        (4, "success", true, false),
        (1, "success", false, false),
        (1, "failure", false, false),
    ] {
        let c = ctx([
            ("cancelled()", cancelled.into()),
            ("inputs.legs", V::from(legs)),
            ("needs.check.result", check.into()),
        ]);
        assert_eq!(
            value(text(&jobs["gate"]["if"]), &c).truthy(),
            verdict,
            "{legs} legs, the legs {check}, cancelled {cancelled}"
        );
    }
}

/// A run, for a job's concurrency group.
#[derive(Clone, Copy)]
struct Run {
    event: &'static str,
    pr: u64,
    run: u64,
    workflow: &'static str,
    action: &'static str,
    /// What an edit changed: `title`, `body` or `base`.
    changes: &'static [&'static str],
    /// Inputs other than [`INPUTS`]'.
    inputs: &'static [(&'static str, &'static str)],
    /// The check job's leg.
    leg: u64,
}

const PUSHED: Run = Run {
    event: "pull_request",
    pr: 12,
    run: 1001,
    workflow: "flake",
    action: "synchronize",
    changes: &[],
    inputs: &[],
    leg: 1,
};

/// The inputs a run is called with, unless it says otherwise.
fn inputs() -> Vec<(&'static str, V)> {
    vec![
        ("inputs.runner", "ubuntu-latest".into()),
        ("inputs.install-nix", true.into()),
        ("inputs.eval-apart", false.into()),
        ("inputs.pr-gate", true.into()),
        ("inputs.build-skip", "agents".into()),
        ("inputs.docs-paths", "README.md".into()),
        ("inputs.docs-checks", "".into()),
        ("inputs.legs", 1.into()),
    ]
}

/// A job's concurrency group for a run.
fn group(jobs: &Value, name: &str, r: &Run) -> String {
    let c = &jobs[name]["concurrency"];
    assert_eq!(c["cancel-in-progress"], true, "{name}: {c:?}");
    let pr = r.event == "pull_request";
    let mut c2 = ctx(inputs());
    for (k, v) in r.inputs {
        let key = format!("inputs.{k}");
        let v = match *v {
            "true" => V::Bool(true),
            "false" => V::Bool(false),
            v => v.parse::<u64>().map_or_else(|_| v.into(), V::from),
        };
        assert!(c2.insert(key, v).is_some(), "no input {k}");
    }
    c2.extend(ctx([
        ("github.event_name", r.event.into()),
        (
            "github.event.pull_request.number",
            if pr { r.pr.into() } else { V::Null },
        ),
        ("github.run_id", r.run.into()),
        (
            "matrix.leg",
            if name == "check" {
                r.leg.into()
            } else {
                V::Null
            },
        ),
        ("github.workflow", r.workflow.into()),
        (
            "github.event.action",
            if pr { r.action.into() } else { V::Null },
        ),
        (
            "github.event.changes.base",
            if r.changes.contains(&"base") {
                // An object, which is all a condition asks of it.
                "{ ref: { from: main } }".into()
            } else {
                V::Null
            },
        ),
    ]));
    value(text(&c["group"]), &c2).text()
}

const JOBS: [&str; 5] = ["lane", "check", "gate", "docs", "commits"];
const TITLE: &[&str] = &["title"];
const BODY: &[&str] = &["body"];
const BASE: &[&str] = &["base"];
const TITLE_AND_BASE: &[&str] = &["title", "base"];

/// Each job's concurrency group: a PR's runs share one, so the newer cancels
/// the older; anything else is its run's own, a title or body edit's too;
/// jobs, workflows, PRs and calls with other inputs never share one.
#[test]
fn only_a_prs_newer_run_cancels() {
    let jobs = jobs();
    let g = |name: &str, r: Run| group(&jobs, name, &r);
    for name in JOBS {
        // The PR's next run cancels this one.
        assert_eq!(
            g(name, PUSHED),
            g(
                name,
                Run {
                    run: 1002,
                    ..PUSHED
                }
            ),
            "{name}"
        );
        let others = [
            g(name, Run { pr: 13, ..PUSHED }),
            g(
                name,
                Run {
                    workflow: "other",
                    ..PUSHED
                },
            ),
            g(
                name,
                Run {
                    inputs: &[("runner", "remi")],
                    ..PUSHED
                },
            ),
            g(
                name,
                Run {
                    inputs: &[("build-skip", "")],
                    ..PUSHED
                },
            ),
            g(
                name,
                Run {
                    inputs: &[("eval-apart", "true")],
                    ..PUSHED
                },
            ),
            g(
                name,
                Run {
                    inputs: &[("pr-gate", "false")],
                    ..PUSHED
                },
            ),
            g(
                name,
                Run {
                    inputs: &[("install-nix", "false")],
                    ..PUSHED
                },
            ),
            g(
                name,
                Run {
                    inputs: &[("docs-paths", "docs/**")],
                    ..PUSHED
                },
            ),
            g(
                name,
                Run {
                    inputs: &[("docs-checks", "links")],
                    ..PUSHED
                },
            ),
            g(
                name,
                Run {
                    inputs: &[("legs", "4")],
                    ..PUSHED
                },
            ),
        ];
        assert!(!others.contains(&g(name, PUSHED)), "{name}: {others:?}");
        let mut distinct = others.to_vec();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), others.len(), "{name}: {others:?}");
    }
    let mut all: Vec<_> = JOBS.iter().map(|n| g(n, PUSHED)).collect();
    all.sort();
    all.dedup();
    assert_eq!(all.len(), JOBS.len());
    // A run's legs never cancel each other.
    let mut legs: Vec<_> = (1..=4)
        .map(|leg| {
            g(
                "check",
                Run {
                    inputs: &[("legs", "4")],
                    leg,
                    ..PUSHED
                },
            )
        })
        .collect();
    legs.sort();
    legs.dedup();
    assert_eq!(legs.len(), 4, "{legs:?}");
    // A push or a train candidate is a run of its own: none cancels another.
    let push = Run {
        event: "push",
        ..PUSHED
    };
    assert_ne!(
        g("check", Run { run: 1001, ..push }),
        g("check", Run { run: 1002, ..push })
    );
    assert_ne!(g("check", Run { run: 12, ..push }), g("check", PUSHED));
    for name in JOBS {
        let pushed = g(name, PUSHED);
        // Renovate pushes and renames a second later: the rename's run
        // leaves the push's alone, whichever starts first, and another
        // edit's.
        for changes in [TITLE, BODY] {
            let edit = |run| Run {
                run,
                action: "edited",
                changes,
                ..PUSHED
            };
            assert_ne!(g(name, edit(1002)), pushed, "{name} {changes:?}");
            assert_ne!(
                g(name, edit(1002)),
                g(name, edit(1003)),
                "{name} {changes:?}"
            );
        }
        // A retarget changes what the head is checked against, so it
        // supersedes the last run, and the next push supersedes it.
        for changes in [BASE, TITLE_AND_BASE] {
            let retarget = Run {
                run: 1002,
                action: "edited",
                changes,
                ..PUSHED
            };
            assert_eq!(g(name, retarget), pushed, "{name} {changes:?}");
        }
        for action in ["opened", "reopened", "synchronize"] {
            assert_eq!(
                g(
                    name,
                    Run {
                        run: 1002,
                        action,
                        ..PUSHED
                    }
                ),
                pushed,
                "{name} {action}"
            );
        }
    }
}

/// The checkout fetches every branch for a PR, then the merge comes before
/// the gate checks anything, and the docs lane's job and the gate job start
/// with the very same two steps.
#[test]
fn the_merge_comes_first() {
    let jobs = jobs();
    let job = &jobs["check"];
    let names: Vec<&str> = steps(job)
        .iter()
        .map(|s| {
            s["id"]
                .as_str()
                .or_else(|| s["uses"].as_str()?.split('@').next())
                .unwrap_or("")
        })
        .collect();
    assert_eq!(names[..2], ["actions/checkout", "merge"], "{names:?}");
    assert!(
        names
            .iter()
            .position(|n| *n == "gate")
            .is_some_and(|i| i > 1),
        "{names:?}"
    );
    assert_eq!(
        steps(job)[0]["with"],
        yaml(
            r#"
            persist-credentials: false
            ref: "${{ github.event.pull_request.head.sha }}"
            fetch-depth: "${{ github.event_name == 'pull_request' && '0' || '1' }}"
            "#
        )
    );
    let merge = &steps(job)[1];
    assert_eq!(merge["if"], "github.event_name == 'pull_request'");
    assert_eq!(merge["shell"], "bash");
    assert_eq!(
        merge["env"],
        yaml(
            r#"
            BASE: "${{ github.event.pull_request.base.ref }}"
            HEAD_SHA: "${{ github.event.pull_request.head.sha }}"
            LEGS: "${{ inputs.legs }}"
            TIP: "${{ needs.lane.outputs.tip }}"
            "#
        )
    );
    // YAML aliases.
    for other in ["docs", "gate"] {
        assert_eq!(steps(&jobs[other])[..2], steps(job)[..2], "{other}");
    }
}

/// GitHub: main, and a PR branch off it.
struct Repo {
    s: Scratch,
    origin: PathBuf,
    work: PathBuf,
    head: String,
}

impl Repo {
    fn new() -> (Repo, String) {
        let s = Scratch::default();
        let (origin, work) = (s.join("origin.git"), s.join("work"));
        s.git(
            s.path(),
            &["init", "-q", "--bare", origin.to_str().unwrap()],
        );
        s.git(s.path(), &["init", "-q", work.to_str().unwrap()]);
        let event_base = s.commit(&work, "a", "base\n");
        s.git(&work, &["push", "-q", origin.to_str().unwrap(), "main"]);
        s.git(&work, &["checkout", "-qb", "feat"]);
        let head = s.commit(&work, "b", "feature\n");
        s.git(&work, &["push", "-q", origin.to_str().unwrap(), "feat"]);
        let repo = Repo {
            s,
            origin,
            work,
            head,
        };
        (repo, event_base)
    }

    /// main moves on, writing `file`: its new tip.
    fn main_moves(&self, file: &str, text: &str) -> String {
        self.s.git(&self.work, &["checkout", "-q", "main"]);
        let tip = self.s.commit(&self.work, file, text);
        self.s.git(
            &self.work,
            &["push", "-q", self.origin.to_str().unwrap(), "main"],
        );
        tip
    }

    /// The checkout as the action leaves it for a PR (every branch fetched,
    /// the head checked out), then the merge step.
    fn run(&self) -> (Ran, PathBuf) {
        self.run_legs("1", "")
    }

    /// The same, on `legs` legs, with the base's tip as the lane job found
    /// it.
    fn run_legs(&self, legs: &str, tip: &str) -> (Ran, PathBuf) {
        let ws = self.s.mkdtemp();
        let (origin, at) = (self.origin.to_str().unwrap(), ws.to_str().unwrap());
        self.s
            .git(self.s.path(), &["clone", "-q", "--no-checkout", origin, at]);
        self.s.git(&ws, &["checkout", "-q", "--detach", &self.head]);
        let jobs = jobs();
        let merge = script(&steps(&jobs["check"])[1]);
        let ran = self.s.bash(
            merge,
            &ws,
            &[
                ("BASE", "main"),
                ("HEAD_SHA", &self.head),
                ("LEGS", legs),
                ("TIP", tip),
            ],
        );
        (ran, ws)
    }

    fn parents(&self, ws: &Path) -> Vec<String> {
        self.s
            .git(ws, &["cat-file", "commit", "HEAD"])
            .lines()
            .filter_map(|l| Some(l.strip_prefix("parent ")?.to_owned()))
            .collect()
    }
}

/// While the run waited, main moved on. It checks the head merged with main
/// as main is now, in GitHub's parent order, and says so.
#[test]
fn a_pr_is_checked_merged_with_its_base_as_it_is_now() {
    let (r, event_base) = Repo::new();
    let now = r.main_moves("c", "later\n");
    assert_ne!(now, event_base);
    let (ran, ws) = r.run();
    assert_eq!(ran.code, 0, "{}", ran.said());
    let head = &r.head;
    assert!(
        ran.stdout
            .contains(&format!("checking {head} merged with main at {now}")),
        "{}",
        ran.said()
    );
    assert_eq!(r.parents(&ws), [now, head.clone()]);
    assert_eq!(std::fs::read_to_string(ws.join("c")).unwrap(), "later\n");
    assert_eq!(std::fs::read_to_string(ws.join("b")).unwrap(), "feature\n");
    assert_eq!(r.s.git(&ws, &["status", "--porcelain"]), "");
}

/// main moved to something the head conflicts with: the job fails, says
/// why, and leaves no merge to check.
#[test]
fn a_head_that_conflicts_with_its_base_fails() {
    let (r, _) = Repo::new();
    r.main_moves("b", "other\n");
    let (ran, ws) = r.run();
    assert_ne!(ran.code, 0, "{}", ran.said());
    assert!(
        ran.stdout
            .contains("::error title=Conflicts with its base::")
            && ran.stdout.contains(&r.head),
        "{}",
        ran.said()
    );
    assert!(r.parents(&ws).len() < 2);
}

/// With legs, it checks the head merged with main where the lane job found
/// it, however main has moved since; and fails, merging nothing, when the
/// lane job didn't say where, or where it said isn't there.
#[test]
fn with_legs_the_base_is_where_the_lane_job_found_it() {
    let (r, event_base) = Repo::new();
    r.main_moves("c", "later\n");
    let (ran, ws) = r.run_legs("4", &event_base);
    assert_eq!(ran.code, 0, "{}", ran.said());
    let head = &r.head;
    assert!(
        ran.stdout
            .contains(&format!("checking {head} merged with main at {event_base}")),
        "{}",
        ran.said()
    );
    assert_eq!(r.parents(&ws), [event_base, head.clone()]);
    assert!(!ws.join("c").exists());
    for (tip, why) in [
        ("", "the lane job didn't say where main was"),
        (&"0".repeat(40), "isn't in this checkout"),
    ] {
        let (ran, ws) = r.run_legs("4", tip);
        assert!(
            ran.code != 0
                && ran.stdout.contains("::error title=No base to merge with::")
                && ran.stdout.contains(why),
            "{tip:?}: {}",
            ran.said()
        );
        assert_eq!(r.s.git(&ws, &["rev-parse", "HEAD"]), *head);
    }
}

/// Every leg, whenever it starts, and a leg run again, checks one commit,
/// with one rev and lastModified: the merge is dated when the newer of the
/// two was committed, whatever the clock says now.
#[test]
fn every_leg_checks_one_commit_whenever_it_starts() {
    let (r, event_base) = Repo::new();
    r.main_moves("c", "later\n");
    let (first, ws) = r.run_legs("4", &event_base);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let (again, ws2) = r.run_legs("4", &event_base);
    assert!(
        first.code == 0 && again.code == 0,
        "{} {}",
        first.said(),
        again.said()
    );
    let merged = r.s.git(&ws, &["rev-parse", "HEAD"]);
    assert_eq!(r.s.git(&ws2, &["rev-parse", "HEAD"]), merged);
    let newer = [event_base.as_str(), r.head.as_str()]
        .iter()
        .map(|c| {
            r.s.git(&ws, &["show", "-s", "--format=%ct", c])
                .parse::<u64>()
                .unwrap()
        })
        .max()
        .unwrap();
    assert_eq!(
        r.s.git(&ws, &["show", "-s", "--format=%at %ct", &merged]),
        format!("{newer} {newer}")
    );
}
