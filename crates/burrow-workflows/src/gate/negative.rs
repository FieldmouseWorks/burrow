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

use super::{Gate, lines};
use crate::harness::{read, repo_file, script, step, steps, text, workflow, write, yaml};

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
    in
    {{
      checks.${{system}} = {{
        t = drv "t" [ (copy ./lib.txt) (copy ./tests) ];
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
    /// says the PR needs one, as the workflow runs them.
    fn run(&self, head: &str, skip: &str, build_fail: &str) -> Ran {
        let g = &self.g;
        for suffix in ["", ".links", ".slot", ".roots", ".overlap"] {
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
                ("BUILD_FAIL".into(), build_fail.into()),
                ("RUNNER_TEMP".into(), path(&temp)),
                ("TOOLS".into(), path(&repo_file(".github/flake-check"))),
                ("BASE".into(), "main".into()),
                ("HEAD_SHA".into(), head.into()),
                ("SKIP".into(), skip.into()),
                ("PATHS_ONLY".into(), paths_only.into()),
            ]
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
    let ran = r.run(&head, "", "-t.drv");
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    assert_eq!(ran.list("tests"), ["tests/want.txt"]);
    assert_eq!(ran.list("checks"), ["t"]);
    let builds = ran.builds();
    assert_eq!(builds.len(), 1, "{builds:?}");
    assert!(
        builds[0].contains("-t.drv^*") && !builds[0].contains("-other.drv"),
        "{builds:?}"
    );
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

/// A PR whose new test passes on the old code too doesn't test its change:
/// red, and it says what to do.
#[test]
fn a_test_that_passes_on_the_old_code_is_red() {
    let r = Repo::new();
    let head = r.pr(
        "vacuous",
        &[
            ("lib.txt", Some("2\n")),
            ("tests/want.txt", Some("1\n# also\n")),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert_eq!(ran.result(), "pass");
    assert!(
        ran.out.contains("The new tests pass on the old code"),
        "{}",
        ran.out
    );
    assert_eq!(ran.list("checks"), ["t"]);
}

/// Test files no check reads test nothing: red, with nothing built.
#[test]
fn tests_no_check_reads_are_red() {
    let r = Repo::new();
    let head = r.pr(
        "unread",
        &[("lib.txt", Some("2\n")), ("docs/tests/x.txt", Some("x\n"))],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert_eq!(ran.result(), "pass");
    assert!(
        ran.out.contains("No check runs the new tests"),
        "{}",
        ran.out
    );
    assert!(ran.builds().is_empty(), "{:?}", ran.calls);
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
    let ran = r.run(&head, "", "-t.drv");
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
