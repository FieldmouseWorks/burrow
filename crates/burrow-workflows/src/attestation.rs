//! The shared CI's attestation step, its own script run with curl and gh
//! stood in for: every repo's job signs its gate's result with its own
//! token, whose audience names the digest of the statement it keeps beside
//! the token (FieldmouseWorks/Conary, docs/ci-attestation.md), the only
//! evidence Conary's train reads, a public repo's too, whose result
//! actions/attest also bundles; a failed gate is attested as failed, and a
//! run with no OIDC token attests nothing. With legs, no leg attests: the
//! gate job does, with the same steps, and passes only when every leg passed
//! on its commit, each sharing out one plan, its log the legs' own, each as
//! its leg wrote it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use crate::harness::{Scratch, read, script, step, steps, workflow, write, yaml};

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
    // The leg inherits the caller's OIDC and attestations grants, checked
    // for every shipped caller by keep_builds' permission-chain test.
    assert!(job.get("permissions").is_none());
    let (at, step) = subject(&job);
    assert_eq!(step["shell"], "bash");
    // Every repo's push and PR, a private one's included; with legs, the
    // gate job's, not a leg's.
    assert_eq!(
        step["if"],
        "${{ !cancelled() && steps.gate.outcome != 'skipped' && inputs.legs <= 1 && (github.event_name == 'push' || github.event_name == 'pull_request') }}"
    );
    // Whether the repo is private doesn't decide whether it signs: Conary's
    // train reads only the token, a public repo's candidates' too.
    assert!(step["env"].get("PRIVATE").is_none(), "{:?}", step["env"]);
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
            ("PR_HEAD", &pr_head),
            ("CURL_LOG", curl_log.to_str().unwrap()),
            ("CURL_FAIL", if c.token_fails { "1" } else { "" }),
            ("MERGE_BASE", c.merge_base),
            ("GH_TOKEN", "t"),
        ];
        if c.oidc {
            env.push(("ACTIONS_ID_TOKEN_REQUEST_URL", c.token_url));
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

/// A run of the step: a push with a passing gate and an OIDC token, unless
/// it says otherwise. Public or private, a repo's run signs the same.
struct Case<'a> {
    outcome: &'a str,
    event: &'a str,
    oidc: bool,
    token_fails: bool,
    /// Where the job asks for its token.
    token_url: &'a str,
    /// What gh says the PR left its base at.
    merge_base: &'a str,
    /// The PR's head as the event gives it, when it isn't the merge's.
    pr_head: Option<&'a str>,
}

impl Default for Case<'_> {
    fn default() -> Self {
        Case {
            outcome: "success",
            event: "push",
            oidc: true,
            token_fails: false,
            token_url: "https://token.example/idtoken?api-version=2.0",
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

/// A push: the subject, the predicate on one line, the token. Its audience
/// names the statement's digest, the request carries the run's own request
/// token, and the attestation's artifact shares the log's uuid. A public
/// repo's too: Conary's train reads only this, never the Sigstore bundle
/// (Conary's docs/ci-attestation.md, "Checking one"), and the steps after
/// this one leave the bundle to a public repo alone.
#[test]
fn a_push_attests_with_its_own_token() {
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

/// GitHub's token service sometimes answers with a 503 (a LoK-web PR's
/// passing gate, 2026-11-05): the job asks again, as it would after any
/// answer that says try later, rather than fail a gate that passed.
#[test]
fn a_token_request_answered_try_later_is_asked_again() {
    let r = Repo::new();
    // The machine's curl, past the stand-in, which still logs what it's asked.
    r.s.stub(
        "curl",
        "printf '%s\\n' \"$@\" >> \"$CURL_LOG\"\nPATH=${PATH#*:} exec curl \"$@\"\n",
    );
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/idtoken?api-version=2.0",
        server.local_addr().unwrap()
    );
    let asked = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = asked.clone();
    std::thread::spawn(move || {
        use std::io::{BufRead, BufReader, Write};
        for mut conn in server.incoming().flatten() {
            let mut request = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            while request.read_line(&mut line).is_ok_and(|n| n > 2) {
                line.clear();
            }
            let answer = if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_owned()
            } else {
                let body = format!("{{\"count\": 1, \"value\": \"{TOKEN}\"}}");
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            };
            let _ = conn.write_all(answer.as_bytes());
        }
    });
    let a = r.run(Case {
        token_url: &url,
        ..Case::default()
    });
    assert_eq!(a.code, 0, "{}", a.said);
    let (_, token) = statement_of(a.attested.as_deref().expect("an attestation"));
    assert_eq!(token, TOKEN);
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// A PR: the revision, the merge step's merge of the head onto the base
/// branch's tip.
#[test]
fn a_pr_attests_its_revision() {
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

/// With legs: each leg's log goes up as an artifact named for it, with its
/// digest, its commit and its plan's digest as the leg's output, one name per
/// leg, which the job passes on. The gate job, once every leg has finished,
/// passed or failed, checks out and merges as they did, judges them, and
/// attests with the very same steps as the gate's job with one leg, but for
/// whether it runs.
#[test]
fn with_legs_the_gate_job_attests_for_them() {
    let wf = workflow(".github/workflows/flake-check.yml");
    let (job, gate) = (&wf["jobs"]["check"], &wf["jobs"]["gate"]);
    let all = steps(job);
    let (at, subject_step) = subject(job);
    let leg_at = all
        .iter()
        .position(|s| s["id"] == "leg")
        .expect("a leg step");
    let (leg, leg_up) = (&all[leg_at], &all[leg_at + 1]);
    assert!(all[leg_at - 1]["id"] == "gate" && leg_at < at, "{all:?}");
    assert_eq!(
        leg["if"],
        "${{ !cancelled() && steps.gate.outcome != 'skipped' && inputs.legs > 1 }}"
    );
    assert_eq!(leg["env"], yaml("LEG: '${{ matrix.leg }}'"));
    assert!(
        leg_up["uses"]
            .as_str()
            .is_some_and(|u| u.starts_with("actions/upload-artifact@")),
        "{leg_up:?}"
    );
    assert_eq!(
        leg_up["if"],
        "${{ !cancelled() && steps.leg.outputs.name }}"
    );
    assert_eq!(
        leg_up["with"],
        yaml("{name: '${{ steps.leg.outputs.name }}', path: '${{ runner.temp }}/flake.log'}")
    );
    let outputs: String = (1..=8)
        .map(|k| format!("leg-{k}: '${{{{ steps.leg.outputs.leg-{k} }}}}'\n"))
        .collect();
    assert_eq!(job["outputs"], yaml(&outputs));
    assert_eq!(gate["needs"], yaml("[lane, reuse, check]"));
    // Or, with no leg run, gate-reuse's hit (reuse.rs).
    assert_eq!(
        gate["if"],
        "${{ !cancelled() && inputs.legs > 1 && (needs.check.result == 'success' || needs.check.result == 'failure' || needs.reuse.outputs.hit == 'true') }}"
    );
    assert_eq!(
        gate["permissions"],
        yaml("contents: read\nid-token: write\nattestations: write")
    );
    let g = steps(gate);
    assert_eq!(g[..2], all[..2]);
    assert!(
        g[2]["uses"]
            .as_str()
            .is_some_and(|u| u.starts_with("actions/download-artifact@")),
        "{:?}",
        g[2]
    );
    assert_eq!(
        g[2]["with"],
        yaml("{pattern: flake-log-leg-*, path: '${{ runner.temp }}/legs'}")
    );
    // gate-reuse: a hit builds and combines nothing, and the gate this run
    // is gets named in the predicate, from the step the legs' jobs have
    // (reuse.rs).
    assert_eq!(g[2]["if"], "needs.reuse.outputs.hit != 'true'");
    assert_eq!(g[3]["id"], "tools");
    assert_eq!(g[4], *step(job, "identity"));
    let verdict = &g[5];
    assert!(
        verdict["id"] == "gate"
            && verdict["shell"] == "bash"
            && verdict["if"] == "needs.reuse.outputs.hit != 'true'",
        "{verdict:?}"
    );
    assert_eq!(
        verdict["env"],
        yaml(
            "{LEGS: '${{ inputs.legs }}', RESULT: '${{ needs.check.result }}', \
             OUTPUTS: '${{ toJSON(needs.check.outputs) }}'}"
        )
    );
    assert_eq!(g[6]["id"], "subject");
    let mut same = g[6].clone();
    same["if"] = subject_step["if"].clone();
    assert_eq!(&same, subject_step);
    assert_eq!(
        g[6]["if"],
        "${{ !cancelled() && steps.gate.outcome != 'skipped' && (github.event_name == 'push' || github.event_name == 'pull_request') }}"
    );
    // The log, the token and the bundle, as a leg attests them; the kept
    // builds (keep-builds) that follow them in a leg are the leg's alone.
    assert_eq!(g[7..10], all[at + 1..at + 4]);
    // Then what reports a hit, as a leg's own job does with one leg.
    assert_eq!(g[10..], [step(job, "reused").clone()]);
}

const LEGS: usize = 3;
const LOGS: [&str; LEGS] = ["first leg\n", "second leg\n", "third leg\n"];

/// What a run of the legs does to them, by leg: none, unless it says so.
#[derive(Default)]
struct Legs<'a> {
    /// The check job's result, as GitHub gives it: success unless it says.
    result: Option<&'a str>,
    /// Whose log is changed after the leg wrote it.
    tamper: Option<usize>,
    /// Whose output names another commit.
    wrong_commit: Option<usize>,
    /// Whose output never reaches the gate job.
    missing: Option<usize>,
    /// Whose output names another leg's log.
    rename: Option<usize>,
    /// Who shared out another plan.
    other_plan: Option<usize>,
    /// Who stopped before it read the plan.
    no_plan: Option<usize>,
}

/// Each leg's step, then the gate job's, as GitHub gives it the legs'
/// combined outputs and their artifacts: its exit code, what it said and the
/// log it leaves to attest.
fn legs(r: &Repo, l: Legs) -> (i32, String, String) {
    let wf = workflow(".github/workflows/flake-check.yml");
    let leg_step = step(&wf["jobs"]["check"], "leg");
    let verdict = step(&wf["jobs"]["gate"], "gate");
    let runner_temp = r.s.mkdtemp();
    let commit = r.rev("HEAD^{commit}");
    let mut outputs = serde_json::Map::new();
    for k in 1..=LEGS {
        let leg_temp = r.s.mkdtemp();
        write(&leg_temp.join("flake.log"), LOGS[k - 1]);
        if l.no_plan != Some(k) {
            let plan = if l.other_plan == Some(k) {
                r#"{"entries": []}"#
            } else {
                r#"{"entries": [1]}"#
            };
            write(&leg_temp.join("plan.json"), plan);
        }
        let leg = k.to_string();
        let ran = r.s.bash(
            script(leg_step),
            &r.repo,
            &[("RUNNER_TEMP", leg_temp.to_str().unwrap()), ("LEG", &leg)],
        );
        assert_eq!(ran.code, 0, "{}", ran.said());
        let said: BTreeMap<&str, &str> = ran
            .outputs
            .lines()
            .map(|l| l.split_once('=').expect("name=value"))
            .collect();
        let name = said["name"];
        let uuid = name
            .strip_prefix(&format!("flake-log-leg-{k}-"))
            .unwrap_or_else(|| panic!("{name}"));
        assert!(
            uuid.len() == 36 && uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
            "{name}"
        );
        let mine = format!("leg-{k}");
        assert_eq!(
            said.keys().copied().collect::<Vec<_>>(),
            [mine.as_str(), "name"]
        );
        if l.missing == Some(k) {
            continue;
        }
        let value = said[mine.as_str()];
        let [name_, sha, checked, plan] = value.split(' ').collect::<Vec<_>>()[..] else {
            panic!("{value}")
        };
        assert!(name_ == name && checked == commit, "{value}");
        let want_plan = if l.no_plan == Some(k) {
            "none".to_owned()
        } else {
            Sha256::digest(read(&leg_temp.join("plan.json")).as_bytes())
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect()
        };
        assert_eq!(plan, want_plan, "{value}");
        let mut value = value.to_owned();
        if l.wrong_commit == Some(k) {
            value = format!("{name} {sha} {} {plan}", "0".repeat(40));
        }
        if l.rename == Some(k) {
            value = value.replacen(&format!("leg-{k}-"), "leg-9-", 1);
        }
        outputs.insert(mine, value.into());
        // download-artifact: each artifact in a directory of its own name.
        let tampered = if l.tamper == Some(k) {
            "tampered\n"
        } else {
            ""
        };
        write(
            &runner_temp.join("legs").join(name).join("flake.log"),
            &(read(&leg_temp.join("flake.log")) + tampered),
        );
    }
    let (legs, outputs) = (
        LEGS.to_string(),
        serde_json::Value::Object(outputs).to_string(),
    );
    let ran = r.s.bash(
        script(verdict),
        &r.repo,
        &[
            ("RUNNER_TEMP", runner_temp.to_str().unwrap()),
            ("LEGS", &legs),
            ("RESULT", l.result.unwrap_or("success")),
            ("OUTPUTS", &outputs),
        ],
    );
    (ran.code, ran.said(), read(&runner_temp.join("flake.log")))
}

/// The gate job's log when every leg's is its own: each, under its name.
fn all_logs() -> String {
    (1..=LEGS)
        .map(|k| format!("leg {k} of {LEGS}:\n{}", LOGS[k - 1]))
        .collect()
}

/// With legs, the gate job passes when every leg did, its log theirs.
#[test]
fn with_legs_the_gate_job_passes_when_every_leg_did() {
    let r = Repo::new();
    r.merge_a_pr();
    let (code, said, log) = legs(&r, Legs::default());
    assert_eq!(code, 0, "{said}");
    assert_eq!(log, all_logs());
}

/// A failed leg, a leg's log missing, changed or for another commit, or legs
/// that split different plans, fail the gate job. A leg whose log is its own
/// keeps it in the gate's, however the leg went.
#[test]
fn a_leg_that_failed_or_cant_be_held_to_its_log_fails_the_gate_job() {
    let r = Repo::new();
    r.merge_a_pr();
    for (l, why, own_logs) in [
        (
            Legs {
                result: Some("failure"),
                ..Legs::default()
            },
            "the legs: failure",
            true,
        ),
        (
            Legs {
                missing: Some(2),
                ..Legs::default()
            },
            "leg 2 of 3 left no log",
            false,
        ),
        (
            Legs {
                tamper: Some(3),
                ..Legs::default()
            },
            "leg 3 of 3: its log here isn't the one it wrote",
            false,
        ),
        (
            Legs {
                rename: Some(1),
                ..Legs::default()
            },
            "leg 1 of 3: its log here isn't the one it wrote",
            false,
        ),
        (
            Legs {
                wrong_commit: Some(2),
                ..Legs::default()
            },
            "leg 2 of 3 checked 0000000000000000000000000000000000000000, not",
            false,
        ),
        (
            Legs {
                other_plan: Some(3),
                ..Legs::default()
            },
            "leg 3 of 3 took its share of another plan than leg 1",
            true,
        ),
        (
            Legs {
                other_plan: Some(1),
                ..Legs::default()
            },
            "leg 2 of 3 took its share of another plan than leg 1",
            true,
        ),
        (
            Legs {
                no_plan: Some(2),
                ..Legs::default()
            },
            "leg 2 of 3 stopped before it read the plan",
            true,
        ),
    ] {
        let (code, said, log) = legs(&r, l);
        assert!(code != 0 && said.contains(why), "{why}: {code} {said}");
        if own_logs {
            assert_eq!(log, all_logs(), "{why}");
        }
    }
}
