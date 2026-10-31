//! The shared CI's attestation step, its own script run with curl and gh
//! stood in for: a private repo's job signs its gate's result with its own
//! token, whose audience names the digest of the statement it keeps beside
//! the token (FieldmouseWorks/Conary, docs/ci-attestation.md), a failed gate
//! is attested as failed, a public repo's is left to actions/attest, and a
//! run with no OIDC token attests nothing.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::harness::{Scratch, read, script, steps, workflow, write, yaml};

const TOKEN: &str = "eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJ4In0.c2ln";

fn job() -> yaml_serde::Value {
    workflow(".github/workflows/flake-check.yml")["jobs"]["check"].clone()
}

/// The step, and where it is among the job's.
fn subject(job: &yaml_serde::Value) -> (usize, &yaml_serde::Value) {
    steps(job)
        .iter()
        .enumerate()
        .find(|(_, s)| s["id"] == "subject")
        .expect("a subject step")
}

#[test]
fn every_push_and_pr_attests_and_uploads() {
    let job = job();
    assert_eq!(job["permissions"]["id-token"], "write");
    let (at, step) = subject(&job);
    assert_eq!(step["shell"], "bash");
    // Every repo's push and PR, a private one's included.
    assert_eq!(
        step["if"],
        "${{ !cancelled() && steps.gate.outcome != 'skipped' && (github.event_name == 'push' || github.event_name == 'pull_request') }}"
    );
    assert_eq!(
        step["env"]["PRIVATE"],
        "${{ github.event.repository.private }}"
    );
    assert_eq!(step["env"]["OUTCOME"], "${{ steps.gate.outcome }}");
    // After it: the log, the token's attestation (only when there is one),
    // and the Sigstore bundle, for a public repo only.
    let after = &steps(&job)[at + 1..at + 4];
    let uses = |s: &yaml_serde::Value, action: &str| {
        s["uses"].as_str().is_some_and(|u| u.starts_with(action))
    };
    let (log_up, token_up, bundle) = (&after[0], &after[1], &after[2]);
    assert!(uses(log_up, "actions/upload-artifact@"), "{log_up:?}");
    assert_eq!(log_up["with"]["name"], "${{ steps.subject.outputs.log }}");
    assert!(uses(token_up, "actions/upload-artifact@"), "{token_up:?}");
    assert_eq!(
        token_up["if"],
        "${{ !cancelled() && steps.subject.outputs.attestation }}"
    );
    assert_eq!(
        token_up["with"],
        yaml(
            r#"
            name: "${{ steps.subject.outputs.attestation-name }}"
            path: "${{ steps.subject.outputs.attestation }}"
            "#
        )
    );
    assert!(uses(bundle, "actions/attest@"), "{bundle:?}");
    assert_eq!(
        bundle["if"],
        "${{ !cancelled() && steps.subject.outputs.path && !github.event.repository.private }}"
    );
}

/// A repo, curl answering with a token (every call logged), and gh answering
/// the compare a PR's subject asks for.
struct Repo {
    s: Scratch,
    repo: PathBuf,
    /// main's first commit.
    tip: String,
}

/// What the step did.
struct Attested {
    code: i32,
    said: String,
    outputs: BTreeMap<String, String>,
    /// The file it attested to, if any.
    attested: Option<String>,
    /// curl's arguments.
    curl: Vec<String>,
}

impl Repo {
    fn new() -> Repo {
        let s = Scratch::default();
        s.stub(
            "curl",
            &format!(
                "printf '%s\\n' \"$@\" >> \"$CURL_LOG\"\n[ -z \"$CURL_FAIL\" ] || exit 22\necho '{{\"count\": 1, \"value\": \"{TOKEN}\"}}'\n"
            ),
        );
        s.stub("gh", "echo \"$MERGE_BASE\"\n");
        let repo = s.join("repo");
        std::fs::create_dir(&repo).unwrap();
        s.git(&repo, &["init", "-q"]);
        let tip = s.commit(&repo, "a", "a");
        Repo { s, repo, tip }
    }

    fn rev(&self, what: &str) -> String {
        self.s.git(&self.repo, &["rev-parse", what])
    }

    fn run(&self, c: Case) -> Attested {
        let runner_temp = self.s.mkdtemp();
        write(&runner_temp.join("flake.log"), "checks passed\n");
        let curl_log = runner_temp.join("curl.log");
        write(&curl_log, "");
        let sha = self.rev("HEAD");
        let pr_head = match c.pr_head {
            Some(head) => head.to_owned(),
            None if c.event == "pull_request" => self.rev("HEAD^2"),
            None => String::new(),
        };
        let mut env = vec![
            ("RUNNER_TEMP", runner_temp.to_str().unwrap()),
            ("GITHUB_EVENT_NAME", c.event),
            ("GITHUB_SHA", &sha),
            ("GITHUB_REPOSITORY", "owner/repo"),
            ("OUTCOME", c.outcome),
            ("SKIP", "agents"),
            ("PRIVATE", if c.private { "true" } else { "false" }),
            ("PR_HEAD", &pr_head),
            ("CURL_LOG", curl_log.to_str().unwrap()),
            ("CURL_FAIL", if c.token_fails { "1" } else { "" }),
            ("MERGE_BASE", c.merge_base),
            ("GH_TOKEN", "t"),
        ];
        if c.oidc {
            env.push((
                "ACTIONS_ID_TOKEN_REQUEST_URL",
                "https://token.example/idtoken?api-version=2.0",
            ));
            env.push(("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request-token"));
        }
        let job = job();
        let ran = self.s.bash(script(subject(&job).1), &self.repo, &env);
        let outputs: BTreeMap<String, String> = ran
            .outputs
            .lines()
            .map(|l| {
                let (k, v) = l.split_once('=').expect("name=value");
                (k.to_owned(), v.to_owned())
            })
            .collect();
        let attested = outputs.get("attestation").map(|p| read(p.as_ref()));
        Attested {
            code: ran.code,
            said: ran.said(),
            outputs,
            attested,
            curl: read(&curl_log).lines().map(str::to_owned).collect(),
        }
    }

    /// A PR's merge, as the merge step leaves it: main moved on, then the
    /// PR's head merged onto it. The head.
    fn merge_a_pr(&self) -> String {
        self.s.git(&self.repo, &["checkout", "-q", "-b", "pr"]);
        let head = self.s.commit(&self.repo, "b", "b");
        self.s.git(&self.repo, &["checkout", "-q", "main"]);
        self.s.commit(&self.repo, "c", "c");
        self.s
            .git(&self.repo, &["merge", "-q", "--no-ff", "--no-edit", &head]);
        head
    }
}

/// A run of the step: a private repo's push with a passing gate and an OIDC
/// token, unless it says otherwise.
struct Case<'a> {
    private: bool,
    outcome: &'a str,
    event: &'a str,
    oidc: bool,
    token_fails: bool,
    /// What gh says the PR left its base at.
    merge_base: &'a str,
    /// The PR's head as the event gives it, when it isn't the merge's.
    pr_head: Option<&'a str>,
}

impl Default for Case<'_> {
    fn default() -> Self {
        Case {
            private: true,
            outcome: "success",
            event: "push",
            oidc: true,
            token_fails: false,
            merge_base: "",
            pr_head: None,
        }
    }
}

/// The statement and the token, as a verifier splits the file.
fn statement_of(attested: &str) -> (String, &str) {
    let (statement, token) = attested
        .trim_end_matches('\n')
        .rsplit_once('\n')
        .expect("a token line");
    (format!("{statement}\n"), token)
}

fn predicate(statement: &str, line: usize) -> serde_json::Value {
    serde_json::from_str(statement.lines().nth(line).expect("a predicate")).unwrap()
}

/// A private repo's push: the subject, the predicate on one line, the token.
/// Its audience names the statement's digest, the request carries the run's
/// own request token, and the attestation's artifact shares the log's uuid.
#[test]
fn a_private_repos_push_attests_with_its_own_token() {
    let r = Repo::new();
    let a = r.run(Case::default());
    assert_eq!(a.code, 0);
    let (statement, token) = statement_of(a.attested.as_deref().expect("an attestation"));
    assert_eq!(token, TOKEN);
    let lines: Vec<&str> = statement.lines().collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert_eq!(lines[..2], ["conary commit v1", r.rev("HEAD").as_str()]);
    let p = predicate(&statement, 2);
    assert_eq!(
        p,
        serde_json::json!({"result": "pass", "revision": null, "tree": r.rev("HEAD^{tree}"),
                           "skipped": ["agents"], "log": p["log"]})
    );
    assert_eq!(
        format!("{}{}\n", read(a.outputs["path"].as_ref()), lines[2]),
        statement
    );
    let digest: String = Sha256::digest(statement.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let audience = format!("conary-attestation:v1:sha256:{digest}");
    assert!(
        a.curl
            .contains(&"Authorization: bearer request-token".to_owned()),
        "{:?}",
        a.curl
    );
    let url = a.curl.last().expect("a token request");
    assert_eq!(
        url,
        &format!("https://token.example/idtoken?api-version=2.0&audience={audience}")
    );
    let (host, query) = url
        .strip_prefix("https://")
        .and_then(|u| u.split_once('/'))
        .and_then(|(host, rest)| Some((host, rest.split_once('?')?.1)))
        .expect("a URL with a query");
    assert_eq!(host, "token.example");
    let mut query: Vec<&str> = query.split('&').collect();
    query.sort();
    assert_eq!(query, ["api-version=2.0", &format!("audience={audience}")]);
    let uuid = a.outputs["log"]
        .strip_prefix("flake-log-")
        .expect("a log name");
    assert_eq!(
        a.outputs["attestation-name"],
        format!("conary-attestation-{uuid}")
    );
    assert_eq!(p["log"]["artifact"], a.outputs["log"].as_str());
}

/// A failed gate is attested too, as failed.
#[test]
fn a_failed_gate_attests_a_fail() {
    let r = Repo::new();
    let a = r.run(Case {
        outcome: "failure",
        ..Case::default()
    });
    assert_eq!(a.code, 0);
    let (statement, _) = statement_of(a.attested.as_deref().expect("an attestation"));
    assert_eq!(predicate(&statement, 2)["result"], "fail");
}

/// A public repo's: the subject for actions/attest, no token asked for.
#[test]
fn a_public_repos_is_left_to_the_sigstore_bundle() {
    let r = Repo::new();
    let a = r.run(Case {
        private: false,
        ..Case::default()
    });
    assert!(
        a.code == 0 && a.attested.is_none() && a.curl.is_empty(),
        "{:?}",
        a.curl
    );
    assert_eq!(a.outputs.keys().collect::<Vec<_>>(), ["log", "path"]);
}

/// No OIDC token: nothing attested, and the job goes on.
#[test]
fn a_run_with_no_oidc_token_attests_nothing() {
    let r = Repo::new();
    let a = r.run(Case {
        oidc: false,
        ..Case::default()
    });
    assert!(
        a.code == 0 && a.outputs.is_empty() && a.curl.is_empty(),
        "{:?}",
        a.outputs
    );
}

/// A token GitHub won't give fails the step rather than attest without one.
#[test]
fn a_token_request_that_fails_fails_the_step() {
    let r = Repo::new();
    let a = r.run(Case {
        token_fails: true,
        ..Case::default()
    });
    assert!(
        a.code != 0 && !a.outputs.contains_key("attestation"),
        "{:?}",
        a.outputs
    );
}

/// A private repo's PR: the revision, the merge step's merge of the head
/// onto the base branch's tip.
#[test]
fn a_private_repos_pr_attests_its_revision() {
    let r = Repo::new();
    let head = r.merge_a_pr();
    let a = r.run(Case {
        event: "pull_request",
        merge_base: &r.tip,
        ..Case::default()
    });
    assert_eq!(a.code, 0);
    let (statement, _) = statement_of(a.attested.as_deref().expect("an attestation"));
    let lines: Vec<&str> = statement.lines().collect();
    assert_eq!(
        lines[..3],
        ["conary revision v1", r.tip.as_str(), head.as_str()]
    );
    assert_eq!(
        predicate(&statement, 3)["revision"],
        serde_json::json!({"base": r.tip, "head": head})
    );
}

/// A checked commit that isn't the merge of the PR's head, as the event
/// names it, attests nothing: its revision would be some other head's.
#[test]
fn a_pr_whose_checked_commit_isnt_its_merge_attests_nothing() {
    let r = Repo::new();
    r.merge_a_pr();
    let a = r.run(Case {
        event: "pull_request",
        merge_base: &r.tip,
        pr_head: Some(&r.tip),
        ..Case::default()
    });
    assert!(
        a.code != 0 && a.outputs.is_empty() && a.said.contains("isn't a merge of"),
        "{} {:?}",
        a.said,
        a.outputs
    );
}
