//! What the gate signs when it's the gate that broke: the gate's step and
//! then the one that signs it, as the job runs them. `fail` is a check's, or
//! the flake's, alone; a gate that couldn't write, was killed or never
//! started signs `error`, and why (#1400).

use std::fs;
use std::os::unix::fs::symlink;

use serde_json::Value;

use super::{Gate, Run};
use crate::harness::{read, script, step, workflow};

const CHECKS: &str = r#"{ checks.${system}.a = drv "a"; }"#;
const ENOSPC: &str = "No space left on device";

/// What a run left: what it printed, the predicate signed and the log
/// that's uploaded beside it.
struct Signed {
    said: String,
    predicate: Value,
    log: String,
}

impl Signed {
    fn is_error(&self, why: &str) {
        let p = &self.predicate;
        assert_eq!(p["result"], "error", "{p} {}", self.said);
        let error = p["error"].as_str().unwrap_or_default();
        assert!(error.contains(why), "{why}: {p} {}", self.said);
        assert!(self.log.contains(error), "{error}: {}", self.log);
    }

    fn is_fail(&self) {
        let p = &self.predicate;
        assert_eq!(p["result"], "fail", "{p} {}", self.said);
        assert!(p.get("error").is_none(), "{p}");
    }
}

/// The gate on `outputs`, failing, with the files named in `full` on a
/// device with no room, and then the step that signs it, once the disk has
/// room again.
fn signed(g: &Gate, outputs: &str, full: &[&str], r: Run) -> Signed {
    let d = g.flake(outputs);
    for name in full {
        symlink("/dev/full", g.s.join(name)).unwrap();
    }
    let ran = g.run(&d, r);
    assert_ne!(ran.code, 0, "{}", ran.out);
    for name in full {
        fs::remove_file(g.s.join(name)).unwrap();
    }
    g.s.stub("curl", "echo '{\"value\": \"token\"}'\n");
    let wf = workflow(".github/workflows/flake-check.yml");
    let sha = g.s.git(&d, &["rev-parse", "HEAD"]);
    let subject = g.s.bash(
        script(step(&wf["jobs"]["check"], "subject")),
        &d,
        &[
            ("RUNNER_TEMP", g.s.path().to_str().unwrap()),
            ("GITHUB_EVENT_NAME", "push"),
            ("GITHUB_SHA", &sha),
            ("GITHUB_REPOSITORY", "owner/repo"),
            ("OUTCOME", "failure"),
            ("SKIP", ""),
            (
                "ACTIONS_ID_TOKEN_REQUEST_URL",
                "https://token.example/x?a=1",
            ),
            ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "t"),
        ],
    );
    assert_eq!(subject.code, 0, "{}", subject.said());
    Signed {
        said: ran.out,
        predicate: serde_json::from_str(&read(&g.s.join("conary/predicate.json"))).unwrap(),
        log: read(&g.s.join("flake.log")),
    }
}

/// A log it can't write, whether the checks passed or one failed too: an
/// error, its reason signed, annotated and in the log that's uploaded.
#[test]
fn a_log_with_no_room_is_an_error_and_says_why() {
    for (whole, build_fail) in [(false, ""), (true, ""), (false, "-a.drv")] {
        let g = Gate::new();
        let run = Run {
            whole,
            build_fail,
            ..Run::default()
        };
        let s = signed(&g, CHECKS, &["flake.log"], run);
        s.is_error(ENOSPC);
        assert!(
            s.said
                .lines()
                .any(|l| l.starts_with("::error title=") && l.contains(ENOSPC)),
            "{}",
            s.said
        );
    }
}

/// A file of its own it can't write: the same, and where it goes on without
/// it (the whole-flake fallback) what fails after it is no check's to answer for.
#[test]
fn a_plan_with_no_room_is_an_error_and_says_why() {
    for (whole, skip, build_fail) in [(false, "", ""), (true, "other", ".a")] {
        let g = Gate::new();
        let run = Run {
            whole,
            skip,
            build_fail,
            ..Run::default()
        };
        signed(&g, CHECKS, &["plan.json"], run).is_error(ENOSPC);
    }
}

/// With no room even to say so, the result still isn't `fail`, and the
/// step's annotation has the reason.
#[test]
fn no_room_for_the_result_is_still_no_fail() {
    for (full, build_fail) in [(&["flake.log"][..], ""), (&[][..], "-a.drv")] {
        let g = Gate::new();
        fs::create_dir(g.s.join("gate-result")).unwrap();
        let run = Run {
            build_fail,
            ..Run::default()
        };
        let s = signed(&g, CHECKS, full, run);
        s.is_error("without saying a check failed");
        if build_fail.is_empty() {
            assert!(s.said.contains(ENOSPC), "{}", s.said);
        }
    }
}

/// An evaluation or a build that was killed, and a root nix couldn't make
/// for a derivation it has: none is a check's failure.
#[test]
fn what_was_killed_or_couldnt_be_rooted_is_an_error() {
    let g = Gate::new();
    let run = Run {
        env: &[("EVAL_KILL", "entry = 0")],
        ..Run::default()
    };
    signed(&g, CHECKS, &[], run).is_error("137");

    let g = Gate::new();
    let nix = crate::harness::which("nix");
    g.s.stub(
        "nix",
        &format!(
            "[ \"$1\" != build ] || exit 137\nexec {} \"$@\"\n",
            nix.display()
        ),
    );
    signed(&g, CHECKS, &[], Run::default()).is_error("137");

    let g = Gate::new();
    let real = nix.parent().unwrap().join("nix-instantiate");
    g.s.stub(
        "nix-instantiate",
        &format!(
            "case \"$*\" in *--add-root*) echo '{ENOSPC}' >&2; exit 1 ;; esac\nexec {} \"$@\"\n",
            real.display()
        ),
    );
    signed(&g, CHECKS, &[], Run::default()).is_error("nix-instantiate");
}

/// A gate that ended before its runner started says nothing: an error.
#[test]
fn a_gate_that_never_started_is_an_error() {
    let g = Gate::new();
    g.s.stub("nix-build", &format!("echo '{ENOSPC}' >&2\nexit 1\n"));
    signed(&g, CHECKS, &[], Run::default()).is_error("without saying a check failed");
}

/// What's the change's own stays `fail`: a check that doesn't build or
/// evaluate, a derivation that isn't one, a flake that doesn't evaluate or
/// that the gate refuses, with eval-apart and without.
#[test]
fn a_failing_check_or_flake_is_still_a_fail() {
    let no_drv = r#"{ checks.${system}.a = { type = "derivation"; name = "a"; drvPath = "/nix/store/00000000000000000000000000000000-a.drv"; }; }"#;
    let broken = r#"{ checks.${system}.a = throw "broken"; }"#;
    let no_plan = r#"{ checks = throw "broken"; }"#;
    let refused = r#"{ overlays.default = final: prev: { }; }"#;
    for (whole, outputs, build_fail) in [
        (false, CHECKS, "-a.drv"),
        (true, CHECKS, "-a.drv"),
        (false, broken, ""),
        (true, broken, ""),
        (false, no_plan, ""),
        (true, no_plan, ""),
        (false, no_drv, ""),
        (false, refused, ""),
    ] {
        let g = Gate::new();
        let run = Run {
            whole,
            build_fail,
            ..Run::default()
        };
        signed(&g, outputs, &[], run).is_fail();
    }
    let g = Gate::new();
    let run = Run {
        legs: Some(("3", "2")),
        ..Run::default()
    };
    signed(&g, CHECKS, &[], run).is_fail();
}
