//! A head no CI reported on, built before it's judged: each of the repo's
//! checks run by burrow-proof's worker, here in this process with no fence
//! (the VM test, tests/judge-look.nix, is the unit's half: its user, its
//! read-only view and its missing network), and the look shown what came
//! of it.
use super::look::{Rig, printed, rig};
use super::{ask, judge_request};
use crate::Config;
use burrow_producer::serve::Work;
use burrow_producer::units::{Ended, Units};
use burrow_proof::fence::Fence;
use conary_core::plugin::Trigger;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A command run as it is, with nothing to clear after.
struct Bare;

impl Fence for Bare {
    fn around(&mut self, _: usize, program: &Path, args: &[&str]) -> std::io::Result<Command> {
        let mut c = Command::new(program);
        c.args(args);
        Ok(c)
    }
    fn clear(&mut self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn record(&self) -> Option<PathBuf> {
        None
    }
}

/// Units that run each build's worker in this process, as its unit would,
/// and keep what they were asked.
#[derive(Clone)]
pub(super) struct Builds {
    cfg: Config,
    pub asked: Arc<Mutex<Asked>>,
}

#[derive(Default)]
pub(super) struct Asked {
    pub ran: Vec<String>,
    pub stopped: Vec<String>,
    pub loaded: Vec<String>,
    /// How many looks' folders there were as each build ran.
    pub looks: Vec<usize>,
    /// Whether each unit breaks, keeping nothing of its worker's (a
    /// timeout, a stop).
    pub broke: bool,
}

impl Builds {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            asked: Arc::default(),
        }
    }
}

impl Units for Builds {
    fn run(&self, id: &str, _: &Work, _: Instant) -> Result<Ended, String> {
        let looks = std::fs::read_dir(self.cfg.state.join("looks"))
            .map(|d| d.count())
            .unwrap_or_default();
        let mut asked = self.asked.lock().unwrap();
        asked.ran.push(id.into());
        asked.looks.push(looks);
        if asked.broke {
            return Ok(Ended::Broke("its unit ended failed (timeout)".into()));
        }
        drop(asked);
        let code = burrow_proof::work::work(self.cfg.bench(), id, &mut Bare);
        if code > 1 {
            let log = std::fs::read_to_string(self.cfg.bench().log(id)).unwrap_or_default();
            eprintln!("build {id} exited {code}: {log}");
        }
        Ok(Ended::Exited(code))
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        self.asked.lock().unwrap().stopped.push(id.into());
        Ok(())
    }
    fn loaded(&self) -> Result<Vec<String>, String> {
        Ok(self.asked.lock().unwrap().loaded.clone())
    }
}

impl Rig {
    /// The flow in `REPO` with `proofs` its checks' recipes, once the
    /// producer and its looks have their config, which writes the flow.
    fn proofs(&self, proofs: Value) {
        let path = self.dir.path().join("flow/acme/widget.json");
        let mut flow: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        flow["settings"]["proofs"] = proofs;
        std::fs::write(&path, flow.to_string()).unwrap();
    }

    fn builds(&self) -> std::sync::MutexGuard<'_, Asked> {
        self.builds.asked.lock().unwrap()
    }
}

fn bash(script: &str) -> Value {
    json!({"Run": ["bash", "-c", script]})
}

/// The published judgment of `r`'s head, its flow's checks' recipes
/// `proofs`, and the look's brief.
fn judged(r: &Rig, proofs: Value, why: Vec<Trigger>) -> (Value, String) {
    let p = r.producer();
    r.proofs(proofs);
    let got = ask(r.dir.path(), &p, &judge_request(&r.w, why)).unwrap();
    let posted = r.w.posted();
    assert_eq!(posted.len(), 1, "{posted:?}");
    assert!(posted[0].starts_with("Read the diff"), "{}", posted[0]);
    (
        json!({"answer": got, "posted": posted[0]}),
        r.seen("prompt"),
    )
}

#[test]
fn a_head_no_ci_reported_on_is_built_and_judged_on_its_checks_passing() {
    let r = rig();
    let (got, prompt) = judged(
        &r,
        json!({
            // It runs on the head as it is: the PR's change is there.
            "gate": bash("grep -q 'fn new' src/lib.rs && echo the gate held"),
            "nix": {"Build": "checks.x86_64-linux.gate"},
            "review": {"Job": "a model's check"},
        }),
        vec![Trigger::NoCi],
    );
    // The look is told what ran, how it came out and what it printed, and
    // never that CI is green.
    assert!(!prompt.contains("CI is green"), "{prompt}");
    assert!(prompt.contains("No CI reported on its head"), "{prompt}");
    assert!(
        prompt.contains(
            "- gate: ran `bash -c grep -q 'fn new' src/lib.rs && echo the gate held` and it passed"
        ),
        "{prompt}"
    );
    assert!(prompt.contains("> the gate held"), "{prompt}");
    assert!(
        prompt.contains("- nix: not run here: a flake build goes through the Nix daemon"),
        "{prompt}"
    );
    assert!(prompt.contains("- review: not run here"), "{prompt}");
    // And the judgment records it, whatever the look said.
    let why = got["posted"].as_str().unwrap();
    assert!(
        why.contains("Built by the judge, as no CI reported on its head (offline, in a unit of its own): gate (`bash -c grep -q 'fn new' src/lib.rs && echo the gate held`) passed; nix not run"),
        "{why}"
    );
    assert_eq!(
        got["answer"]["why"].as_str(),
        Some(why.split("\n\n<!--").next().unwrap())
    );
    // One build, the gate's, stopped, before any look was laid out, and its
    // folder gone.
    let builds = r.builds();
    assert_eq!(builds.ran.len(), 1);
    assert_eq!(builds.stopped, builds.ran);
    assert_eq!(builds.looks, [0]);
    assert_eq!(
        std::fs::read_dir(r.dir.path().join("state/runs"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn a_head_no_ci_reported_on_whose_check_fails_is_judged_on_that() {
    let r = rig();
    let (got, prompt) = judged(
        &r,
        json!({
            "gate": bash("echo the gate held"),
            "tests": bash("echo 'test widget ... FAILED'; exit 3"),
        }),
        vec![Trigger::NoCi, Trigger::Asked],
    );
    assert!(
        prompt.contains("- gate: ran `bash -c echo the gate held` and it passed"),
        "{prompt}"
    );
    assert!(
        prompt
            .contains("- tests: ran `bash -c echo 'test widget ... FAILED'; exit 3` and it failed"),
        "{prompt}"
    );
    assert!(prompt.contains("> test widget ... FAILED"), "{prompt}");
    let why = got["posted"].as_str().unwrap();
    assert!(why.contains("gate (`bash -c echo the gate held`) passed; tests (`bash -c echo 'test widget ... FAILED'; exit 3`) failed."), "{why}");
    assert_eq!(r.builds().ran.len(), 2);
}

#[test]
fn a_build_reaches_its_own_copy_of_the_head_and_never_the_looks() {
    let r = rig();
    let looks = r.dir.path().join("state/looks");
    let mirror = r.dir.path().join("state/git/acme/widget.git");
    let github = r.dir.path().join("github/acme/widget.git");
    // Every commit a repo has, and what each holds, sorted: commits made
    // in the same second come out of `git log --all` in an order their
    // history doesn't fix, and it can differ between the two.
    let commits = |repo: &Path| {
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(repo)
            .args(["log", "--all", "--format=%H %T %s"])
            .output()
            .unwrap();
        let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect();
        lines.sort();
        lines.join("\n")
    };
    // It rewrites and commits what it has, and every look's checkout it
    // can find.
    let (_, prompt) = judged(
        &r,
        json!({"gate": bash(&format!(
            "echo 'fn forged() {{}}' >src/lib.rs && git -c user.name=b -c user.email=b@b commit -qam forged \
             && for d in {}/*/head; do echo 'fn forged() {{}}' >\"$d/src/lib.rs\"; done; true",
            looks.display()
        ))}),
        vec![Trigger::NoCi],
    );
    assert!(prompt.contains("and it passed"), "{prompt}");
    // No look was laid out while it ran, and the one laid out after read
    // the head as GitHub has it, from the mirror, which it left as it was.
    assert_eq!(r.builds().looks, [0]);
    let laid = &r.asked().laid;
    assert_eq!(laid.len(), 1);
    assert_eq!(laid[0].1, "fn new() {}\n");
    assert_eq!(commits(&mirror), commits(&github));
    assert!(!commits(&mirror).contains("forged"));
}

#[test]
fn a_check_that_couldnt_run_here_is_shown_as_that_and_never_passes_the_head() {
    // A program the unit doesn't have, as most checks that need the
    // network or Nix are here.
    let proofs = json!({"gate": bash("echo no python3 here >&2; exit 127")});
    let r = rig();
    let p = r.producer();
    r.proofs(proofs.clone());
    // The look passes it anyway: refused, and nothing is posted.
    let got = ask(r.dir.path(), &p, &judge_request(&r.w, vec![Trigger::NoCi]));
    assert!(
        got.unwrap_err().contains("none of whose checks passed"),
        "a pass of a head nothing passed on"
    );
    let prompt = r.seen("prompt");
    assert!(
        prompt.contains(
            "- gate: ran `bash -c echo no python3 here >&2; exit 127` and it couldn't run here"
        ),
        "{prompt}"
    );
    assert!(prompt.contains("> no python3 here"), "{prompt}");
    assert!(r.w.posted().is_empty());

    // Sent to the owner, it's published, with what was built.
    let r = rig();
    r.prints(&printed("owner", "a-model"));
    let (got, _) = judged(&r, proofs, vec![Trigger::NoCi]);
    assert_eq!(got["answer"]["verdict"], "owner", "{got}");
    let why = got["posted"].as_str().unwrap();
    assert!(
        why.contains("gate (`bash -c echo no python3 here >&2; exit 127`) couldn't run here."),
        "{why}"
    );
}

#[test]
fn a_build_whose_unit_keeps_no_outcome_judges_nothing() {
    let r = rig();
    let p = r.producer();
    r.proofs(json!({"gate": bash("echo the gate held")}));
    r.builds().broke = true;
    let got = ask(r.dir.path(), &p, &judge_request(&r.w, vec![Trigger::NoCi]));
    assert!(got.unwrap_err().contains("gate's build broke"));
    assert!(r.asked().ran.is_empty());
    assert!(r.w.posted().is_empty());
}

#[test]
fn a_judge_owed_with_its_reasons_unrecorded_is_shown_the_heads_ci() {
    let r = rig();
    r.w.with(|p| {
        p.check_runs = vec![
            json!({"id": 1, "name": "flake", "status": "completed", "conclusion": "failure"}),
            json!({"id": 2, "name": "conary/judgment", "status": "completed", "conclusion": "success"}),
        ];
        p.statuses = vec![
            json!({"context": "lint", "state": "success", "creator": {"login": "alice", "type": "User"}}),
            json!({"context": "review-settled", "state": "success"}),
        ];
    });
    let (_, prompt) = judged(
        &r,
        json!({"gate": bash("echo the gate held")}),
        vec![Trigger::Started],
    );
    assert!(!prompt.contains("CI is green"), "{prompt}");
    assert!(
        prompt.contains("> flake: failure\n> lint: success (a status, posted by alice)\n"),
        "{prompt}"
    );
    assert!(!prompt.contains("conary/judgment:"), "{prompt}");
    assert!(!prompt.contains("review-settled"), "{prompt}");
    // CI reported, so nothing was built.
    assert!(r.builds().ran.is_empty());

    // With nothing but the judge's own run, the steward's word and a
    // proof's on it, none of them CI, it's built, as a head no CI reported
    // on.
    let r = rig();
    r.w.with(|p| {
        p.check_runs = vec![json!({"id": 2, "name": "conary/judgment", "status": "completed", "conclusion": "success"})];
        p.statuses = vec![
            json!({"context": "review-settled", "state": "success"}),
            json!({"context": "conary/train", "state": "success"}),
            json!({"context": "Gate", "state": "success"}),
        ];
    });
    let (got, prompt) = judged(
        &r,
        json!({"gate": bash("echo the gate held")}),
        vec![Trigger::Started],
    );
    assert!(
        prompt.contains("- gate: ran `bash -c echo the gate held` and it passed"),
        "{prompt}"
    );
    assert!(
        got["posted"]
            .as_str()
            .unwrap()
            .contains("Built by the judge")
    );
    assert_eq!(r.builds().ran.len(), 1);
}

#[test]
fn a_restart_stops_the_builds_it_lost_and_clears_their_folders() {
    let r = rig();
    let looks = r.looks();
    let lost = r.dir.path().join("state/runs/0123abcd");
    std::fs::create_dir_all(lost.join("repo.git")).unwrap();
    r.builds().loaded = vec!["0123abcd".into()];
    assert_eq!(looks.reconcile().unwrap(), 1);
    assert_eq!(r.builds().stopped, ["0123abcd"]);
    assert!(!lost.exists());
}
