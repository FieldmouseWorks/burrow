//! The negative run (flake-check.yml's negative job): a PR's new tests on
//! its base's code. Whether a PR needs one is git's to say, from which test
//! files it changes, by path; then the checks whose derivation the new tests
//! change are built on the negative tree, and it passes only when one of
//! them fails there. What it found is attested under a subject of its own.
//!
//! Each case is a repo whose main has a flake with three checks: `t` reads
//! the code (`lib.txt`) and the tests (`tests/`), `other` the code alone,
//! and `fromtests` is a test file's own expression. Each takes its files as
//! a source filter does, by their contents: a check that takes the whole
//! tree differs wherever any file does, so it's built too. Nix evaluates them for
//! real; builds are the gate's stand-in's, which fail on request.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{Gate, LOGS, lines};
use crate::expr::{V, value};
use crate::harness::{read, repo_file, script, step, steps, text, workflow, write, yaml};

/// Which new tests count as having passed on the old code: those a check
/// that passed runs.
mod runs;

/// t's derivation once its test wants 2, which fails it.
const WANTS_2: &str = r#""want":"2\n""#;

/// A repo with the flake on main, and a clone of it as the job's checkout.
struct Repo {
    g: Gate,
    origin: PathBuf,
    co: PathBuf,
}

/// What the job's steps did.
struct Ran {
    code: i32,
    out: String,
    outputs: BTreeMap<String, String>,
    /// negative.json, if it wrote one.
    found: Option<serde_json::Value>,
    /// nix's calls, as the gate's stand-ins log them.
    calls: Vec<String>,
}

impl Ran {
    fn result(&self) -> &str {
        self.found
            .as_ref()
            .and_then(|f| f["result"].as_str())
            .unwrap_or_else(|| panic!("nothing found:\n{}", self.out))
    }

    fn list(&self, key: &str) -> Vec<&str> {
        self.found.as_ref().unwrap()[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect()
    }

    fn builds(&self) -> Vec<&str> {
        self.calls
            .iter()
            .map(String::as_str)
            .filter(|c| c.starts_with("build "))
            .collect()
    }
}

impl Repo {
    fn new() -> Repo {
        Repo::declaring("{ }")
    }

    /// The same, with t's attributes past its derivation's (`{ testFiles
    /// = [ ]; }`, say) as this Nix expression.
    fn declaring(t_extra: &str) -> Repo {
        let g = Gate::new();
        let origin = g.dir();
        let system = &g.system;
        write(
            &origin.join("flake.nix"),
            &format!(
                r#"{{
  outputs = {{ self }}:
    let
      system = "{system}";
      drv = name: deps: derivation {{ inherit name system; builder = "/bin/sh"; args = [ "-c" "echo ${{toString deps}} > $out" ]; }};
      # A path of the flake's own, as a source filter makes it: by its
      # contents, not the whole tree's.
      copy = path: builtins.path {{ inherit path; name = baseNameOf path; }};
      # What the test wants, in t's derivation, so a case can fail its
      # build by what it holds.
      want = if builtins.pathExists ./tests/want.txt then builtins.readFile ./tests/want.txt else "nothing";
    in
    {{
      checks.${{system}} = {{
        t = derivation {{
          name = "t";
          inherit system want;
          dep = drv "dep" [ ];
          builder = "/bin/sh";
          args = [ "-c" "echo ${{toString [ (copy ./lib.txt) (copy ./tests) ]}} > $out" ];
        }} // {t_extra};
        other = drv "other" [ (copy ./lib.txt) ];
        fromtests = import ./tests/check.nix {{ inherit drv; }};
      }};
    }};
}}
"#
            ),
        );
        write(&origin.join("lib.txt"), "1\n");
        write(&origin.join("tests/want.txt"), "1\n");
        write(
            &origin.join("tests/check.nix"),
            "{ drv }: drv \"fromtests\" [ ]\n",
        );
        g.s.git(&origin, &["init", "-q"]);
        g.s.git(&origin, &["add", "-A"]);
        g.s.git(&origin, &["commit", "-qm", "main"]);
        let co = g.s.join("co");
        g.s.git(
            g.s.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                co.to_str().unwrap(),
            ],
        );
        Repo { g, origin, co }
    }

    /// A PR's head: main with these files written (or, for None, removed),
    /// fetched into the checkout.
    fn pr(&self, name: &str, files: &[(&str, Option<&str>)]) -> String {
        let s = &self.g.s;
        s.git(&self.origin, &["checkout", "-q", "-B", name, "main"]);
        for (f, text) in files {
            match text {
                Some(t) => write(&self.origin.join(f), t),
                None => fs::remove_file(self.origin.join(f)).unwrap(),
            }
        }
        s.git(&self.origin, &["add", "-A"]);
        s.git(&self.origin, &["commit", "-qm", name]);
        let head = s.git(&self.origin, &["rev-parse", "HEAD"]);
        s.git(&self.origin, &["checkout", "-q", "main"]);
        s.git(
            &self.co,
            &[
                "fetch",
                "-q",
                "origin",
                "+refs/heads/*:refs/remotes/origin/*",
            ],
        );
        s.git(&self.co, &["checkout", "-q", "--detach", &head]);
        head
    }

    /// The job's paths step, then its negative step when the paths step
    /// says the PR needs one, as the workflow runs them, with builds of a
    /// derivation whose text holds one of `fail` (space-separated) failing.
    fn run(&self, head: &str, skip: &str, fail: &str) -> Ran {
        self.run_with(head, skip, &[("BUILD_FAIL_TEXT", fail)])
    }

    /// The same, with more of the stand-ins' settings.
    fn run_with(&self, head: &str, skip: &str, more: &[(&str, &str)]) -> Ran {
        let g = &self.g;
        for suffix in LOGS {
            let _ = fs::remove_file(g.s.join(&format!("nix.log{suffix}")));
        }
        let temp = g.s.mkdtemp();
        let wf = workflow(".github/workflows/flake-check.yml");
        let job = &wf["jobs"]["negative"];
        let path = |p: &Path| p.to_str().unwrap().to_owned();
        let env = |paths_only: &str| -> Vec<(String, String)> {
            vec![
                ("NIX_CONFIG".into(), g.nix_config.clone()),
                ("XDG_CACHE_HOME".into(), path(&g.s.join("cache"))),
                ("NIX_LOG".into(), path(&g.log())),
                ("RUNNER_TEMP".into(), path(&temp)),
                ("TOOLS".into(), path(&repo_file(".github/flake-check"))),
                ("BASE".into(), "main".into()),
                ("HEAD_SHA".into(), head.into()),
                ("SKIP".into(), skip.into()),
                ("PATHS_ONLY".into(), paths_only.into()),
            ]
            .into_iter()
            .chain(more.iter().map(|(k, v)| ((*k).into(), (*v).into())))
            .collect()
        };
        let run = |id: &str, paths_only: &str| {
            let env = env(paths_only);
            let env: Vec<(&str, &str)> =
                env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            g.s.bash(script(step(job, id)), &self.co, &env)
        };
        let paths = run("paths", "true");
        let mut code = paths.code;
        let mut out = paths.said();
        let mut outputs = paths.outputs.clone();
        if code == 0 && paths.outputs.contains("needs=true") {
            let neg = run("negative", "false");
            code = neg.code;
            out += &neg.said();
            outputs += &neg.outputs;
        }
        let found = temp.join("negative.json");
        Ran {
            code,
            out,
            outputs: outputs
                .lines()
                .filter_map(|l| l.split_once('='))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
            found: found
                .exists()
                .then(|| serde_json::from_str(&read(&found)).unwrap()),
            calls: lines(&g.log()),
        }
    }

    /// A file of a tree, by its id: none where it has none.
    fn file(&self, tree: &str, f: &str) -> Option<String> {
        let mut cmd = self.g.s.command("git");
        cmd.args(["cat-file", "-p", &format!("{tree}:{f}")])
            .current_dir(&self.co);
        let out = crate::harness::output(cmd);
        out.status
            .success()
            .then(|| String::from_utf8(out.stdout).unwrap())
    }
}

/// The job: on every PR, the head with every branch's history, its files at
/// the workflow's commit, and the patterns of what's a test in its script,
/// never an input a PR's caller could set.
#[test]
fn every_pr_gets_one_from_the_workflows_own_files() {
    let wf = workflow(".github/workflows/flake-check.yml");
    let job = &wf["jobs"]["negative"];
    assert_eq!(job["if"], "github.event_name == 'pull_request'");
    assert_eq!(job["permissions"]["id-token"], "write");
    // On the runner's runner, or the one the caller names for it.
    for (negative, runner) in [("", "remi"), ("fresh-remi", "fresh-remi")] {
        let ctx = [
            ("inputs.runner", V::from("remi")),
            ("inputs.negative-runner", V::from(negative)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect();
        assert_eq!(value(text(&job["runs-on"]), &ctx).text(), runner);
    }
    let st = steps(job);
    assert_eq!(st[0]["with"]["fetch-depth"], 0, "{:?}", st[0]);
    assert_eq!(
        st[0]["with"]["ref"],
        "${{ github.event.pull_request.head.sha }}"
    );
    assert_eq!(
        st[1]["with"],
        yaml(
            "{repository: '${{ job.workflow_repository }}', ref: '${{ job.workflow_sha }}', \
             path: .flake-check, persist-credentials: false, sparse-checkout: .github/flake-check}"
        )
    );
    for (id, paths_only) in [("paths", "true"), ("negative", "false")] {
        let s = step(job, id);
        assert_eq!(s["run"], r#"bash "$TOOLS/negative.sh""#);
        assert_eq!(
            s["env"],
            yaml(&format!(
                "{{BASE: '${{{{ github.event.pull_request.base.ref }}}}', \
                 HEAD_SHA: '${{{{ github.event.pull_request.head.sha }}}}', \
                 SKIP: '${{{{ inputs.build-skip }}}}', PATHS_ONLY: '{paths_only}', \
                 TOOLS: '${{{{ github.workspace }}}}/.flake-check/.github/flake-check'}}"
            )),
            "{id}"
        );
    }
    assert_eq!(
        step(job, "negative")["if"],
        "steps.paths.outputs.needs == 'true'"
    );
    // No input says what a test is.
    let inputs = wf["on"]["workflow_call"]["inputs"].as_mapping().unwrap();
    assert!(
        inputs.keys().all(|k| !text(k).contains("test")),
        "{inputs:?}"
    );
}

/// A PR whose new test fails without its change: only the check that reads
/// the tests is built, on the base's code with the head's tests, and its
/// failure there is the result wanted.
#[test]
fn a_test_that_catches_its_change_fails_on_the_old_code() {
    let r = Repo::new();
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let ran = r.run(&head, "", WANTS_2);
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    assert_eq!(ran.list("tests"), ["tests/want.txt"]);
    assert_eq!(ran.list("checks"), ["t"]);
    // Built, then what it's built from, then the base's, then again on
    // its own: its own builder's failure, on what the new tests change.
    let builds = ran.builds();
    assert_eq!(builds.len(), 4, "{builds:?}");
    assert!(
        builds[0].contains("-t.drv^*") && !builds[0].contains("-other.drv"),
        "{builds:?}"
    );
    assert!(builds[1].contains("-dep.drv^out"), "{builds:?}");
    assert!(
        builds[2].contains("-t.drv^*") && builds[2] != builds[0],
        "{builds:?}"
    );
    assert_eq!(builds[3], builds[0], "{builds:?}");
    // The negative tree: the base's code, the head's tests.
    let tree = ran.found.as_ref().unwrap()["tree"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(r.file(&tree, "lib.txt").as_deref(), Some("1\n"));
    assert_eq!(r.file(&tree, "tests/want.txt").as_deref(), Some("2\n"));
    let fork = r.g.s.git(&r.co, &["rev-parse", "origin/main"]);
    assert_eq!(ran.outputs["fork"], fork);
}

/// A test file whose own expression doesn't evaluate on the old code
/// fails there too.
#[test]
fn tests_that_dont_evaluate_on_the_old_code_fail_there() {
    let r = Repo::new();
    let head = r.pr(
        "eval",
        &[
            ("lib.txt", Some("2\n")),
            (
                "tests/check.nix",
                Some("{ drv }: throw \"needs what the change adds\"\n"),
            ),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    assert!(ran.list("checks").contains(&"fromtests"), "{}", ran.out);
}

/// Tests alone, code alone, or tests only deleted: none needed, and nothing
/// evaluated, nix not even installed for it.
#[test]
fn a_pr_that_adds_no_test_to_code_needs_none() {
    let r = Repo::new();
    for (name, files) in [
        ("tests-only", vec![("tests/want.txt", Some("2\n"))]),
        ("code-only", vec![("lib.txt", Some("2\n"))]),
        (
            "deleted",
            vec![("lib.txt", Some("2\n")), ("tests/want.txt", None)],
        ),
    ] {
        let head = r.pr(name, &files);
        let ran = r.run(&head, "", "");
        assert_eq!(ran.code, 0, "{name}: {}", ran.out);
        assert_eq!(ran.result(), "none", "{name}: {}", ran.out);
        assert_eq!(ran.outputs["needs"], "false", "{name}");
        assert!(ran.found.as_ref().unwrap()["tree"].is_null(), "{name}");
        assert!(ran.calls.is_empty(), "{name}: {:?}", ran.calls);
    }
}

/// A test deleted beside one added: the negative tree hasn't got it.
#[test]
fn a_deleted_test_is_gone_from_the_negative_tree() {
    let r = Repo::new();
    let head = r.pr(
        "swap",
        &[
            ("lib.txt", Some("2\n")),
            ("tests/want.txt", None),
            ("tests/new.txt", Some("2\n")),
        ],
    );
    let ran = r.run(&head, "", r#""want":"nothing""#);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    let tree = ran.found.as_ref().unwrap()["tree"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(r.file(&tree, "tests/want.txt"), None);
    assert_eq!(r.file(&tree, "tests/new.txt").as_deref(), Some("2\n"));
    assert_eq!(r.file(&tree, "lib.txt").as_deref(), Some("1\n"));
    assert_eq!(ran.list("tests"), ["tests/new.txt", "tests/want.txt"]);
}

/// A check that reads the new tests but that build-skip names isn't built:
/// skipped, which the caller's other runner has to settle.
#[test]
fn a_skipped_check_is_named_and_not_built() {
    let r = Repo::new();
    let head = r.pr(
        "skip",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let ran = r.run(&head, "t", "");
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "skipped", "{}", ran.out);
    assert_eq!(ran.list("skipped"), ["t"]);
    assert!(ran.list("checks").is_empty());
    assert!(ran.builds().is_empty(), "{:?}", ran.calls);
}

/// What's a test file, by path, wherever the repo keeps them.
#[test]
fn test_files_are_known_by_their_paths() {
    let r = Repo::new();
    for (f, test) in [
        ("crates/a/src/tests.rs", true),
        ("crates/a/tests/it.rs", true),
        ("tools/x/test_y.py", true),
        ("pkg/z_test.go", true),
        ("z_test.go", true),
        ("tests/load_test.py", true),
        ("pkg/tests/load_test.py", true),
        ("tools/load_test.py", false),
        ("tools/load_test.sh", false),
        ("tools/load_test.rs", false),
        ("web/src/a.test.ts", true),
        ("src/test/java/A.java", true),
        ("src/testing.rs", false),
        ("contest/x.rs", false),
        ("tests.md", false),
    ] {
        let head = r.pr(
            &format!("p{}", f.len()),
            &[("lib.txt", Some("2\n")), (f, Some("x\n"))],
        );
        let ran = r.run(&head, "", "");
        assert_eq!(
            ran.outputs["needs"],
            if test { "true" } else { "false" },
            "{f}: {}",
            ran.out
        );
    }
}

/// What it found, attested: the subject `conary negative v1` names the PR's
/// revision, the predicate the result with the revision and the log, and a
/// private repo's job signs it with its own token.
#[test]
fn what_it_found_is_attested_under_its_own_subject() {
    let r = Repo::new();
    let s = &r.g.s;
    s.stub(
        "curl",
        "printf '%s\\n' \"$@\" >> \"$CURL_LOG\"\necho '{\"count\": 1, \"value\": \"tok.en.sig\"}'\n",
    );
    let wf = workflow(".github/workflows/flake-check.yml");
    let job = &wf["jobs"]["negative"];
    let subject = step(job, "subject");
    assert_eq!(
        subject["if"],
        "${{ !cancelled() && steps.paths.outcome != 'skipped' }}"
    );
    let temp = s.mkdtemp();
    let found =
        r#"{"result":"fail","tree":"abc","tests":["tests/a.txt"],"checks":["t"],"skipped":[]}"#;
    write(&temp.join("negative.json"), found);
    write(&temp.join("negative.log"), "built t: it failed\n");
    let curl_log = temp.join("curl.log");
    let (fork, head) = ("1".repeat(40), "2".repeat(40));
    let ran = s.bash(
        script(subject),
        &r.co,
        &[
            ("RUNNER_TEMP", temp.to_str().unwrap()),
            ("FORK", &fork),
            ("PR_HEAD", &head),
            ("PRIVATE", "true"),
            ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://token.example/?x=1"),
            ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request"),
            ("CURL_LOG", curl_log.to_str().unwrap()),
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
    let attestation = read(&temp.join("conary/conary-attestation"));
    let lines: Vec<&str> = attestation.lines().collect();
    assert_eq!(lines[..3], ["conary negative v1", &fork, &head]);
    let predicate: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(predicate["result"], "fail");
    assert_eq!(
        predicate["revision"],
        serde_json::json!({"base": fork, "head": head})
    );
    assert_eq!(predicate["tree"], "abc");
    assert_eq!(predicate["tests"], serde_json::json!(["tests/a.txt"]));
    assert_eq!(predicate["checks"], serde_json::json!(["t"]));
    let log_sha = hex(&Sha256::digest(b"built t: it failed\n"));
    assert_eq!(predicate["log"]["sha256"], log_sha);
    assert!(
        predicate["log"]["artifact"]
            .as_str()
            .is_some_and(|a| a.starts_with("negative-log-")),
        "{predicate}"
    );
    assert_eq!(lines[4], "tok.en.sig");
    assert_eq!(lines.len(), 5);
    // The audience names the digest of the statement before the token.
    let statement = format!("{}\n", lines[..4].join("\n"));
    let digest = hex(&Sha256::digest(statement.as_bytes()));
    assert!(
        read(&curl_log).contains(&format!("audience=conary-attestation:v1:sha256:{digest}")),
        "{}",
        read(&curl_log)
    );
    // Nothing to attest when the run couldn't tell.
    fs::remove_file(temp.join("negative.json")).unwrap();
    fs::remove_dir_all(temp.join("conary")).unwrap();
    let ran = s.bash(
        script(subject),
        &r.co,
        &[
            ("RUNNER_TEMP", temp.to_str().unwrap()),
            ("FORK", &fork),
            ("PR_HEAD", &head),
            ("PRIVATE", "true"),
            ("ACTIONS_ID_TOKEN_REQUEST_URL", "https://token.example/?x=1"),
            ("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request"),
            ("CURL_LOG", curl_log.to_str().unwrap()),
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(!temp.join("conary/conary-attestation").exists());
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A failure that isn't the new tests' own says nothing, so nothing's
/// attested and the job fails: a check that fails on the base too, one
/// whose dependencies didn't build, or one the base can't evaluate.
#[test]
fn a_failure_that_isnt_the_tests_own_cant_tell() {
    let r = Repo::new();
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let both = r.run(&head, "", r#""name":"t""#);
    let deps = r.run(&head, "", &format!(r#"{WANTS_2} "name":"dep""#));
    for (ran, says) in [(both, "fails too"), (deps, "didn't build")] {
        assert_eq!(ran.code, 1, "{says}: {}", ran.out);
        assert!(ran.found.is_none(), "{says}: {}", ran.out);
        assert!(ran.out.contains(says), "{says}: {}", ran.out);
    }
    // Broken on main: nothing to compare with.
    let flake = read(&r.origin.join("flake.nix"));
    let broken = flake.replace(
        r#"other = drv "other" [ (copy ./lib.txt) ];"#,
        r#"other = throw "broken on main";"#,
    );
    assert_ne!(broken, flake);
    write(&r.origin.join("flake.nix"), &broken);
    r.g.s.git(&r.origin, &["commit", "-qam", "break other"]);
    let head = r.pr(
        "on-broken",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let ran = r.run(&head, "", WANTS_2);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(
        ran.out.contains("doesn't evaluate on the base"),
        "{}",
        ran.out
    );
}

/// An evaluation that failed reaching the daemon, even where nix places
/// the error in code, evaluates apart from it.
#[test]
fn an_evaluation_the_store_failed_says_nothing() {
    let r = Repo::new();
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let t = format!(r#"negative#. --apply o: o.checks.{}."t""#, r.g.system);
    let ran = r.run_with(&head, "", &[("EVAL_DAEMON", &t)]);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(ran.out.contains("evaluated on a second try"), "{}", ran.out);
}

/// A check that failed only while a dependency did, or whose builder a
/// signal stopped, says nothing either: the last try, once everything it's
/// built from has built, is what counts, and only its own builder's exit.
#[test]
fn only_the_checks_own_builder_failing_counts() {
    let r = Repo::new();
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let dep = r#""name":"dep""#;
    // Failing in the build and its retry, then building: the old order
    // counted it once the dependency built.
    let twice = r.run_with(&head, "", &[("BUILD_FLAKY_TEXT", dep)]);
    assert_eq!(twice.code, 1, "{}", twice.out);
    assert!(twice.found.is_none(), "{}", twice.out);
    assert!(twice.out.contains("didn't build"), "{}", twice.out);
    // Failing once, then building: then the check, on its own, passes.
    let once = r.run_with(
        &head,
        "",
        &[("BUILD_FLAKY_TEXT", dep), ("BUILD_FLAKY_TIMES", "1")],
    );
    assert_eq!(once.code, 1, "{}", once.out);
    assert_eq!(once.result(), "pass", "{}", once.out);
    assert!(once.out.contains("it passes"), "{}", once.out);
    // Killed, or exiting as a shell does for a signal or a timeout.
    let killed = r.run_with(&head, "", &[("BUILD_SIGNAL_TEXT", WANTS_2)]);
    let shell = r.run_with(
        &head,
        "",
        &[("BUILD_FAIL_TEXT", WANTS_2), ("BUILD_EXIT", "137")],
    );
    let timeout = r.run_with(
        &head,
        "",
        &[("BUILD_FAIL_TEXT", WANTS_2), ("BUILD_EXIT", "124")],
    );
    for ran in [killed, shell, timeout] {
        assert_eq!(ran.code, 1, "{}", ran.out);
        assert!(ran.found.is_none(), "{}", ran.out);
        assert!(
            ran.out
                .contains("own builder failed with a test's exit code"),
            "{}",
            ran.out
        );
    }
    // A test's own code counts.
    let code = r.run_with(
        &head,
        "",
        &[("BUILD_FAIL_TEXT", WANTS_2), ("BUILD_EXIT", "101")],
    );
    assert_eq!(code.result(), "fail", "{}", code.out);
}

/// A check that's a fetch can fail on the network: it says nothing.
#[test]
fn a_fetch_failing_cant_tell() {
    let r = Repo::new();
    let flake = read(&r.origin.join("flake.nix"));
    let fetch = flake.replace(
        r#"inherit system want;"#,
        &format!(
            r#"inherit system want; outputHashMode = "flat"; outputHash = "sha256-" + (if want == "2
" then "{}" else "{}");"#,
            "A".repeat(43) + "=",
            "B".repeat(42) + "E="
        ),
    );
    assert_ne!(fetch, flake);
    write(&r.origin.join("flake.nix"), &fetch);
    r.g.s.git(&r.origin, &["commit", "-qam", "t a fetch"]);
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let ran = r.run(&head, "", WANTS_2);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(ran.out.contains("is a fetch"), "{}", ran.out);
}

/// A check nix's JSON can't say is or isn't a fetch says nothing either.
#[test]
fn a_check_not_known_not_to_be_a_fetch_cant_tell() {
    let r = Repo::new();
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let ran = r.run_with(
        &head,
        "",
        &[("BUILD_FAIL_TEXT", WANTS_2), ("DRV_SHOW_BREAK", "-t.drv")],
    );
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(
        ran.out.contains("can't tell whether") && ran.out.contains("is a fetch"),
        "{}",
        ran.out
    );
}

/// What a check that failed is built from builds as the check did: in the
/// heavy slot when there's a VM to run in it.
#[test]
fn its_dependencies_wait_for_the_slot_too() {
    let r = Repo::new();
    let flake = read(&r.origin.join("flake.nix"));
    let vm = flake.replace(
        r#"dep = drv "dep" [ ];"#,
        r#"dep = derivation { name = "dep"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; requiredSystemFeatures = [ "kvm" ]; };"#,
    );
    assert_ne!(vm, flake);
    write(&r.origin.join("flake.nix"), &vm);
    r.g.s.git(&r.origin, &["commit", "-qam", "a VM for t"]);
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let (_, path) = super::slots::heavy(&r.g);
    let ran = r.run_with(&head, "", &[("BUILD_FAIL_TEXT", WANTS_2), ("PATH", &path)]);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    let in_slot = lines(&r.g.s.join("nix.log.slot"));
    assert!(
        in_slot
            .iter()
            .any(|b| b.contains("-dep.drv^out") && b.contains("--max-jobs")),
        "{in_slot:?}\n{}",
        ran.out
    );
}

/// An evaluation that failed with a fetch or a build on the way, with an
/// error nix doesn't place in code (nix out of memory), or that a signal
/// stopped the second time, didn't fail on what it evaluated.
#[test]
fn an_evaluation_counts_only_failing_on_its_own_expression() {
    let r = Repo::new();
    let throws = r.pr(
        "throws",
        &[
            ("lib.txt", Some("2\n")),
            (
                "tests/check.nix",
                Some("{ drv }: throw \"needs what the change adds\"\n"),
            ),
        ],
    );
    let fromtests = format!(
        r#"negative#. --apply o: o.checks.{}."fromtests""#,
        r.g.system
    );
    let again = r.run_with(&throws, "", &[("EVAL_KILL_AGAIN", &fromtests)]);
    let oom = r.run_with(&throws, "", &[("EVAL_OOM", &fromtests)]);

    let fetches = r.pr(
        "fetches",
        &[
            ("lib.txt", Some("2\n")),
            (
                "tests/check.nix",
                Some(
                    "{ drv }: drv \"fromtests\" [ (builtins.fetchurl { url = \"http://127.0.0.1:9/x\"; sha256 = \"0000000000000000000000000000000000000000000000000000\"; }) ]\n",
                ),
            ),
        ],
    );
    let config = format!("{}\ndownload-attempts = 1", r.g.nix_config);
    let fetched = r.run_with(&fetches, "", &[("NIX_CONFIG", &config)]);
    for (ran, says) in [
        (again, "ended with 137"),
        (oom, "doesn't place its error"),
        (fetched, "fetched on the way"),
    ] {
        assert_eq!(ran.code, 1, "{says}: {}", ran.out);
        assert!(ran.found.is_none(), "{says}: {}", ran.out);
        assert!(ran.out.contains(says), "{says}: {}", ran.out);
    }
}

/// A flake input the store hasn't got yet is fetched by whichever
/// evaluation needs it first. When that's the negative tree's failing one,
/// its failure is still the test's own: the second try, which fetches
/// nothing, is the one held to it. And when it came from a substituter,
/// as nixpkgs does on a fresh runner, which leaves no copy the fetchers
/// keep, the second try still fetches nothing: the inputs are copied
/// into its store first.
#[test]
fn an_input_the_failing_evaluation_fetched_first_still_counts() {
    let r = Repo::new();
    let s = &r.g.s;
    // The input: a tarball, served over HTTP as often as it's asked for.
    let www = s.join("www");
    write(&www.join("dep/x.txt"), "x\n");
    let mut tar = s.command("tar");
    tar.args(["czf", "dep.tar.gz", "dep"]).current_dir(&www);
    assert!(crate::harness::output(tar).status.success());
    let tarball = fs::read(www.join("dep.tar.gz")).unwrap();
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/dep.tar.gz", server.local_addr().unwrap());
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for mut conn in server.incoming().flatten() {
            let mut request = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            while request.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                tarball.len()
            );
            let _ = conn.write_all(head.as_bytes());
            let _ = conn.write_all(&tarball);
        }
    });
    let flake = read(&r.origin.join("flake.nix"))
        .replace(
            "  outputs = { self }:",
            &format!("  inputs.dep = {{ url = \"{url}\"; flake = false; }};\n  outputs = {{ self, dep }}:"),
        )
        .replace("{ inherit drv; }", "{ inherit drv dep; }");
    write(&r.origin.join("flake.nix"), &flake);
    write(
        &r.origin.join("tests/check.nix"),
        "{ drv, ... }: drv \"fromtests\" [ ]\n",
    );
    // Locked with a cache of its own, then collected, so neither the job's
    // store nor its fetcher cache has the input.
    let nix = |args: &[&str]| {
        let mut cmd = s.command(args[0]);
        cmd.args(&args[1..])
            .current_dir(&r.origin)
            .env("NIX_CONFIG", &r.g.nix_config)
            .env("XDG_CACHE_HOME", s.join("lock-cache"));
        let out = crate::harness::output(cmd);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    };
    s.git(&r.origin, &["add", "-A"]);
    nix(&["nix", "flake", "lock"]);
    // A binary cache holding the input, for the second run.
    let archived: serde_json::Value =
        serde_json::from_str(&nix(&["nix", "flake", "archive", "--json"])).unwrap();
    let dep_path = archived["inputs"]["dep"]["path"]
        .as_str()
        .unwrap()
        .to_owned();
    let bincache = format!("file://{}", s.join("bincache").display());
    nix(&["nix", "copy", "--to", &bincache, &dep_path]);
    nix(&["nix-store", "--gc"]);
    s.git(&r.origin, &["add", "-A"]);
    s.git(&r.origin, &["commit", "-qm", "an input"]);
    let head = r.pr(
        "needsdep",
        &[
            ("lib.txt", Some("2\n")),
            (
                "tests/check.nix",
                Some(
                    "{ drv, dep }: builtins.seq (builtins.readFile \"${dep}/x.txt\") (throw \"needs what the change adds\")\n",
                ),
            ),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    assert!(ran.list("checks").contains(&"fromtests"), "{}", ran.out);
    // Again with neither the store nor the fetchers' cache holding it,
    // and the binary cache to substitute it from.
    nix(&["nix-store", "--gc"]);
    fs::remove_dir_all(s.join("cache")).unwrap();
    let config = r.g.nix_config.replace(
        "substituters =",
        &format!("require-sigs = false\nsubstituters = {bincache}"),
    );
    assert_ne!(config, r.g.nix_config);
    let ran = r.run_with(&head, "", &[("NIX_CONFIG", &config)]);
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
}

/// An evaluation a signal or a limit stopped didn't fail on the old code.
#[test]
fn a_killed_evaluation_cant_tell() {
    let r = Repo::new();
    let head = r.pr(
        "killed",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let ran = r.run_with(&head, "", &[("EVAL_KILL", "/negative#.")]);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(ran.out.contains("ended with 137"), "{}", ran.out);
}

/// It evaluates side by side only as far as the job's memory goes, as the
/// gate does: with a limit it can't read, one at a time.
#[test]
fn it_evaluates_within_the_jobs_memory() {
    let r = Repo::new();
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let proc = r.g.s.mkdtemp();
    write(&proc.join("cgroup"), "0::/job\n");
    let ran = r.run_with(
        &head,
        "",
        &[
            ("BUILD_FAIL_TEXT", WANTS_2),
            ("CGROUP_PROC", proc.to_str().unwrap()),
        ],
    );
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    assert!(
        ran.out.contains("memory limit is unknown") && ran.out.contains("one evaluation at a time"),
        "{}",
        ran.out
    );
}
