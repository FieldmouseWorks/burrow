//! The judge plugin, its producer and its looks, against one scripted
//! GitHub that Conary's reads and burrow's writes both reach (so what the
//! producer posts is what it reads back), scripted looks or a stand-in
//! engine, and the kit's sockets as the flow and the producer reach them.
use super::*;
use world::{Pr, World, github, read, world};

mod build;
mod look;
mod ran;
mod world;

use crate::discussion::{Discussion, highest_generation};
use crate::producer::{CHECK, Judges, Producer, comment};
use burrow_producer::serve::{Controller, Server, Work};
use burrow_producer::sign::{Judgment as Kept, Signer};
use conary_core::op::{Outcome, Repo};
use conary_core::plugin::{
    Context, Executed, Identity, Judgment, Provenance, Request, Response, Trigger,
};
use conary_github::api::{CheckRun, Comment};
use conary_github::attest::service::Producers;
use conary_github::attest::{Subject, Trust, attested};
use conary_github::flow::{self, Basis, Revision, SealKey, ThreadComment};
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const REPO: &str = "acme/widget";
const PR: u64 = 5;
const HEAD: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";
const SECRET: [u8; 32] = [7; 32];
const PRODUCER: &str = "machine-judge";
const FLOW: &str = "flow-bot";
const SEAL: &str = "abababababababababababababababababababababababababababababababab";
/// Where the flow's configurations find the seal key: the controller's
/// copy, which the producer never reads.
const FLOW_KEY: &str = "/run/autoland-flow-keys/judge-seal";
const AUTHOR: &str = "author-session";
const REVIEWER: &str = "reviewer-session";

/// Settings where the judging role runs Claude's one model at one effort.
fn settings(dir: &Path) -> burrow_settings::Config {
    let overlay = dir.join("overlay.json");
    std::fs::write(
        overlay.with_extension("roles.json"),
        json!({"routing": {"models": {"judge claude": {"model": "a-model", "effort": "high"}}},
            "offers": {"claude": {"models": ["a-model"], "efforts": ["high"]}}, "migrated": 1})
        .to_string(),
    )
    .unwrap();
    serde_json::from_value(json!({"overlay": overlay})).unwrap()
}

/// The config, with the flow in `REPO` reading verdicts from `FLOW` under
/// the seal key `key` gives (`judge_key`, `judge_key_file` or both).
fn config_with(dir: &Path, key: serde_json::Value) -> Config {
    let mut flow = key;
    flow["verdict_authors"] = json!([FLOW]);
    let flows = dir.join("flow");
    std::fs::create_dir_all(&flows).unwrap();
    std::fs::write(flows.join("acme/widget.json".replace('/', "_")), "unused").unwrap();
    std::fs::create_dir_all(flows.join("acme")).unwrap();
    std::fs::write(
        flows.join("acme/widget.json"),
        json!({"settings": flow}).to_string(),
    )
    .unwrap();
    Config {
        socket: dir.join("judge.sock"),
        controller: "flow".into(),
        producer: PRODUCER.into(),
        credential: "judge-key".into(),
        seal: "judge-seal".into(),
        flow: flows,
        flow_key: FLOW_KEY.into(),
        settings: settings(dir),
        role: "judge".into(),
        owner: "Sam".into(),
        api: "http://127.0.0.1:1".into(),
        token: "true".into(),
        gh: "gh".into(),
        git: "git".into(),
        github: dir.join("github").to_string_lossy().into_owned(),
        state: dir.join("state"),
        unit: "autoland-judge-look".into(),
        systemctl: "systemctl".into(),
        timeout: 60,
        claude: dir.join("claude"),
        login: "claude-token".into(),
        hosts: vec!["api.anthropic.com".into()],
        build: "autoland-judge-build".into(),
        trees: dir.join("trees"),
    }
}

/// As the module renders it: the flow names the key's file.
fn config(dir: &Path) -> Config {
    config_with(dir, json!({"judge_key_file": FLOW_KEY}))
}

fn seal() -> SealKey {
    SealKey::from_hex(SEAL).unwrap()
}

/// The describe snapshot a v5 request carries: what the flow negotiated, so
/// any model and effort a test's flow chooses.
fn negotiated() -> Value {
    json!({"by": {"engine": "claude", "session": null},
        "offers": {"models": [{"id": "a-model", "efforts": ["high", "low"]},
            {"id": "b-model", "efforts": ["high", "low"]}],
            "capabilities": {"sandbox": "brings", "network": true, "devices": [], "mcp": false},
            "meters": []}})
}

fn judge_request(w: &World, why: Vec<Trigger>) -> Request {
    serde_json::from_value(json!({
        "contract": conary_core::plugin::CONTRACT,
        "execution": {"choice": {"engine": "claude", "model": "a-model", "effort": "high"},
            "context": {"kind": "fresh"}},
        "negotiated": negotiated(),
        "kind": "judge", "repo": REPO, "pr": PR, "head": w.with(|p| p.head.clone()),
        "against": w.against(), "author": {"engine": "claude", "session": AUTHOR},
        "reviewers": [{"engine": "codex", "session": REVIEWER}],
        "why": why, "context": w.context(),
    }))
    .unwrap()
}

fn answer(session: &str, verdict: Judgment) -> Response {
    Response {
        contract: conary_core::plugin::CONTRACT.into(),
        by: Identity {
            engine: "claude".into(),
            session: Some(session.into()),
        },
        execution: Some(Executed {
            model: Some("a-model".into()),
            effort: Some("high".into()),
            context: Context::Fresh,
        }),
        answer: conary_core::plugin::Answer::Judge {
            verdict,
            why: "Built it, read every thread: sound.".into(),
        },
    }
}

/// Looks that run nothing: it answers what the test scripts, in turn,
/// doing to the PR first what the test says, and keeps what it was asked.
#[derive(Clone, Default)]
struct Scripted(Arc<Mutex<Script>>);

#[derive(Default)]
struct Script {
    asked: Vec<Request>,
    answers: VecDeque<Response>,
    during: Option<Box<dyn FnOnce() + Send>>,
    /// Whether it answers only once its request has ended.
    wait_gone: bool,
}

impl Judges for Scripted {
    fn judge(&self, request: &Request, work: &Work) -> Result<Response, String> {
        let mut s = self.0.lock().unwrap();
        s.asked.push(request.clone());
        if let Some(f) = s.during.take() {
            f();
        }
        if s.wait_gone {
            while !work.abandoned() {
                thread::sleep(Duration::from_millis(10));
            }
        }
        s.answers
            .pop_front()
            .ok_or_else(|| "nothing scripted".into())
    }
}

impl Scripted {
    fn answering(answers: Vec<Response>) -> Self {
        Self(Arc::new(Mutex::new(Script {
            answers: answers.into(),
            ..Default::default()
        })))
    }
}

fn producer(dir: &Path, w: &World, judges: Scripted) -> Producer {
    Producer::new(
        config(dir),
        read(w),
        github(w),
        Box::new(judges),
        Signer::for_tests(PRODUCER, SECRET),
        seal(),
    )
    .unwrap()
}

/// A request's work, as the kit's socket gives it: handled by `p` behind a
/// socket, through the plugin, as the flow asks.
fn through_socket(dir: &Path, p: Producer, request: &Request) -> Result<Value, String> {
    let me = Controller(rustix::process::getuid().as_raw());
    let server = Server::bind(&dir.join("judge.sock"), me).unwrap();
    let serving = thread::spawn(move || {
        server
            .once(&move |request, work: &Work| p.handle(request, work))
            .unwrap();
    });
    let cfg = config(dir);
    let mut out = Vec::new();
    let asked = serde_json::to_vec(request).unwrap();
    let answered = plugin(&cfg)
        .unwrap()
        .answer(&mut asked.as_slice(), &mut out);
    serving.join().unwrap();
    answered.map(|()| serde_json::from_slice(&out).unwrap())
}

/// What Conary's verifier says of a `conary/judgment` run with `text` on
/// commit `commit`: its result and log.
fn verify(text: &str, commit: &str) -> Result<(Outcome, Option<String>), String> {
    let public = ed25519_dalek::SigningKey::from_bytes(&SECRET).verifying_key();
    use base64::Engine as _;
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.as_bytes());
    let producers = Producers::parse(
        json!([{"name": PRODUCER, "key": key, "checks": [CHECK]}])
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let trust = Trust {
        signer: "Acme/ci/.github/workflows/gate.yml".parse().unwrap(),
        fresh: None,
        producers,
    };
    let run: CheckRun = serde_json::from_value(json!({
        "id": 1, "name": CHECK, "status": "completed", "conclusion": "success",
        "html_url": null, "started_at": null, "check_suite": null,
        "output": {"text": text}}))
    .unwrap();
    let github =
        conary_github::Api::new("http://127.0.0.1:1", conary_github::Tokens::given("unused"));
    let repo: Repo = REPO.parse().unwrap();
    attested(
        &github,
        &github,
        &repo,
        Subject::Commit {
            commit,
            tree: TREE,
            definition: None,
        },
        &run,
        &trust,
    )
    .unwrap()
    .map(|a| (a.result, a.log))
}

/// `request`, handled by `handler` behind the kit's socket in `dir`, from a
/// client that sends it as the plugin does: the answer, or the refusal. A
/// client given `hang_up` closes its connection when told, which is the
/// request ending.
fn ask_with(
    dir: &Path,
    handler: &dyn burrow_producer::serve::Handler,
    request: &Request,
    hang_up: Option<std::sync::mpsc::Receiver<()>>,
) -> Result<Value, String> {
    let socket = dir.join("ask.sock");
    let me = Controller(rustix::process::getuid().as_raw());
    let server = Server::bind(&socket, me).unwrap();
    let mut line = serde_json::to_vec(request).unwrap();
    line.push(b'\n');
    let client = thread::spawn(move || {
        let mut stream = UnixStream::connect(&socket).unwrap();
        std::io::Write::write_all(&mut stream, &line).unwrap();
        if let Some(told) = hang_up {
            told.recv().unwrap();
            return Err("hung up".to_string());
        }
        let mut reply = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stream), &mut reply).unwrap();
        let reply: Value = serde_json::from_str(&reply).unwrap();
        match reply.get("answer") {
            Some(answer) => Ok(answer.clone()),
            None => Err(reply["refused"].as_str().unwrap().to_owned()),
        }
    });
    server.once(handler).unwrap();
    client.join().unwrap()
}

fn ask(dir: &Path, p: &Producer, request: &Request) -> Result<Value, String> {
    ask_with(dir, &|r, w: &Work| p.handle(r, w), request, None)
}

fn generations(w: &World) -> u64 {
    let comments: Vec<Comment> =
        serde_json::from_value(json!(w.with(|p| p.comments.clone()))).unwrap();
    highest_generation(&comments, &BTreeSet::from([FLOW.to_owned()]))
}

#[test]
fn the_plugin_offers_what_the_judging_roles_engine_offers() {
    let dir = tempfile::tempdir().unwrap();
    let p = plugin(&config(dir.path())).unwrap();
    assert_eq!(p.by.engine, "claude");
    let Answer::Describe { models, kinds, .. } = &p.describe else {
        panic!("{:?}", p.describe)
    };
    assert_eq!(kinds, &[Kind::Judge]);
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "a-model");
}

#[test]
fn a_judgment_is_sealed_with_its_why_and_context_and_signed_over_its_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let why = vec![Trigger::R2, Trigger::RoundLimit];
    let request = judge_request(&w, why.clone());
    let judges = Scripted::answering(vec![answer("judge-1", Judgment::Pass)]);
    let p = producer(dir.path(), &w, judges.clone());
    let got = through_socket(dir.path(), p, &request).unwrap();
    assert_eq!(got["verdict"], "pass");
    assert_eq!(got["by"]["session"], "judge-1");
    assert_eq!(judges.0.lock().unwrap().asked.len(), 1);

    // The sealed comment is exactly Conary's issuance of what was judged:
    // the request's why and context, on the PR's revision, generation 1.
    let basis = Basis {
        why: why.clone(),
        context: w.context(),
    };
    let provenance = Provenance {
        by: answer("judge-1", Judgment::Pass).by,
        execution: answer("judge-1", Judgment::Pass).execution.unwrap(),
    };
    let issued = |generation, verdict, revision: &Revision| {
        seal()
            .judgment_by(
                REPO,
                PR,
                verdict,
                revision,
                generation,
                provenance.clone(),
                basis.clone(),
            )
            .unwrap()
    };
    let posted = w.posted();
    assert_eq!(posted.len(), 1);
    assert!(posted[0].ends_with(&format!("\n\n{}", issued(1, Judgment::Pass, &w.revision()))));
    assert!(
        posted[0].contains(r#""why":["r2","round_limit"]"#),
        "{}",
        posted[0]
    );
    assert!(posted[0].contains(&w.context()));
    assert_eq!(generations(&w), 1);
    // Another generation, verdict or revision is another issuance.
    let moved = Revision {
        base: "other".into(),
        ..w.revision()
    };
    for other in [
        issued(2, Judgment::Pass, &w.revision()),
        issued(1, Judgment::Fail, &w.revision()),
        issued(1, Judgment::Pass, &moved),
    ] {
        assert!(!posted[0].contains(&other));
    }

    // Then conary/judgment, signed over the receipt that binds the same.
    let completed = w.completed();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0].0, "success");
    let receipt = |generation, verdict, revision: &Revision, basis: &Basis| {
        flow::receipt_digest(&flow::judgment_receipt_by(
            revision,
            generation,
            Some(verdict),
            Some(&provenance),
            Some(basis),
        ))
    };
    let (result, log) = verify(&completed[0].1, HEAD).unwrap();
    assert_eq!(result, Outcome::Pass);
    let log = log.unwrap();
    assert_eq!(log, receipt(1, Judgment::Pass, &w.revision(), &basis));
    // The statement is for that generation, verdict, revision, why and
    // context alone.
    let fewer = Basis {
        why: vec![Trigger::R2],
        ..basis.clone()
    };
    let other_context = Basis {
        context: "4".repeat(64),
        ..basis.clone()
    };
    for other in [
        receipt(2, Judgment::Pass, &w.revision(), &basis),
        receipt(1, Judgment::Fail, &w.revision(), &basis),
        receipt(1, Judgment::Pass, &moved, &basis),
        receipt(1, Judgment::Pass, &w.revision(), &fewer),
        receipt(1, Judgment::Pass, &w.revision(), &other_context),
    ] {
        assert_ne!(log, other);
    }
    // On another commit, or with its log edited, it fails the verifier.
    assert!(verify(&completed[0].1, &"5".repeat(40)).is_err());
    let edited = completed[0]
        .1
        .replace(&log, &receipt(2, Judgment::Pass, &w.revision(), &basis));
    assert_ne!(edited, completed[0].1);
    assert!(verify(&edited, HEAD).is_err());
}

#[test]
fn a_judge_session_that_is_the_authors_a_reviewers_or_a_past_judges_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let judges = Scripted::answering(vec![
        answer(AUTHOR, Judgment::Pass),
        answer(REVIEWER, Judgment::Pass),
        answer("judge-1", Judgment::Pass),
        answer("judge-1", Judgment::Pass),
    ]);
    let p = producer(dir.path(), &w, judges);
    let request = judge_request(&w, vec![Trigger::Asked]);
    for _ in 0..2 {
        let refused = ask(dir.path(), &p, &request).unwrap_err();
        assert!(refused.contains("can't judge this"), "{refused}");
    }
    assert!(w.posted().is_empty());
    ask(dir.path(), &p, &request).unwrap();
    // A fresh judgment in a session that judged before isn't fresh.
    let refused = ask(dir.path(), &p, &request).unwrap_err();
    assert!(refused.contains("can't judge this"), "{refused}");
    assert_eq!(w.posted().len(), 1);
    // A refused one started no check run: the one judgment's is the only
    // one, and the latest.
    let conclusions: Vec<String> = w.completed().into_iter().map(|c| c.0).collect();
    assert_eq!(conclusions, ["success"]);
    assert_eq!(w.with(|p| p.runs.len()), 2);
}

#[test]
fn a_commit_carries_one_prs_judgments() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let other = |w: &World| {
        let mut r = judge_request(w, vec![Trigger::Asked]);
        let Task::Judge { pr, .. } = &mut r.task else {
            unreachable!()
        };
        *pr = 6;
        r
    };
    let runs = |w: &World| w.with(|p| p.runs.len());
    // #6 is judged while #5, on the same commit, has its own judged and
    // published: #6's would be the commit's latest run and hide #5's
    // receipt, so it's refused before it commits.
    let five = Scripted::answering(vec![answer("judge-5", Judgment::Pass)]);
    let p5 = producer(dir.path(), &w, five);
    let elsewhere = tempfile::tempdir().unwrap();
    let six = Scripted::answering(vec![answer("judge-6", Judgment::Pass)]);
    let (during, sock, request) = (
        w.clone(),
        elsewhere.path().to_owned(),
        judge_request(&w, vec![Trigger::Asked]),
    );
    six.0.lock().unwrap().during = Some(Box::new(move || {
        ask(&sock, &p5, &request).unwrap();
        assert_eq!(during.posted().len(), 1);
    }));
    let p6 = producer(dir.path(), &w, six.clone());
    let refused = ask(dir.path(), &p6, &other(&w)).unwrap_err();
    assert!(refused.contains("carries #5's judgment"), "{refused}");
    assert!(w.with(|p| p.others.is_empty()));
    assert_eq!(runs(&w), 2);
    let completed = w.completed();
    assert_eq!(completed.len(), 1);
    assert!(verify(&completed[0].1, HEAD).is_ok());
    // Once #5's is on it, #6 is refused before any work, a restart or not.
    for _ in 0..2 {
        let judges = Scripted::answering(vec![answer("judge-7", Judgment::Pass)]);
        let p = producer(dir.path(), &w, judges.clone());
        let refused = ask(dir.path(), &p, &other(&w)).unwrap_err();
        assert!(refused.contains("carries #5's judgment"), "{refused}");
        assert!(judges.0.lock().unwrap().asked.is_empty());
    }
    assert!(w.with(|p| p.others.is_empty()));
    assert_eq!(runs(&w), 2);
    // #5 itself is judged again on it.
    let judges = Scripted::answering(vec![answer("judge-8", Judgment::Fail)]);
    let p = producer(dir.path(), &w, judges);
    ask(dir.path(), &p, &judge_request(&w, vec![Trigger::Asked])).unwrap();
    assert_eq!(w.posted().len(), 2);
    assert_eq!(runs(&w), 4);

    // A judgment committed whose publication failed holds its commit too.
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    w.with(|p| p.refusing = 1);
    let judges = Scripted::answering(vec![
        answer("judge-5", Judgment::Pass),
        answer("judge-6", Judgment::Pass),
    ]);
    let p = producer(dir.path(), &w, judges);
    assert!(ask(dir.path(), &p, &judge_request(&w, vec![Trigger::Asked])).is_err());
    let refused = ask(dir.path(), &p, &other(&w)).unwrap_err();
    assert!(refused.contains("carries #5's judgment"), "{refused}");
    assert!(w.with(|p| p.others.is_empty() && p.runs.is_empty()));
}

#[test]
fn a_judgment_overtaken_while_it_was_judged_publishes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let judges = Scripted::answering(vec![answer("judge-1", Judgment::Pass)]);
    let during = w.clone();
    judges.0.lock().unwrap().during = Some(Box::new(move || {
        during.with(|p| {
            p.comments
                .push(json!({"user": {"login": FLOW, "type": "Bot"},
                "body": "<!-- autoland-judgment:v3 {\"issued\":{\"generation\":2}} -->"}))
        })
    }));
    let p = producer(dir.path(), &w, judges);
    let refused = ask(dir.path(), &p, &judge_request(&w, vec![Trigger::Asked])).unwrap_err();
    assert!(refused.contains("superseded"), "{refused}");
    assert_eq!(w.posted().len(), 1);
    assert!(w.with(|p| p.runs.is_empty()));
}

#[test]
fn a_judgment_whose_publication_failed_goes_out_before_the_prs_next() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    w.with(|p| p.refusing = 1);
    let judges = Scripted::answering(vec![
        answer("judge-1", Judgment::Fail),
        answer("judge-2", Judgment::Pass),
    ]);
    let p = producer(dir.path(), &w, judges);
    let request = judge_request(&w, vec![Trigger::Asked]);
    // Its comment wasn't taken: committed, it waits, with no run.
    assert!(ask(dir.path(), &p, &request).is_err());
    assert!(w.posted().is_empty());
    assert!(w.with(|p| p.runs.is_empty()));
    // The next one sends it first, so the newest run is the newest
    // generation's.
    ask(dir.path(), &p, &request).unwrap();
    let posted = w.posted();
    assert_eq!(posted.len(), 2);
    assert!(posted[0].contains(r#""generation":1"#), "{}", posted[0]);
    assert!(posted[1].contains(r#""generation":2"#), "{}", posted[1]);
    let completed = w.completed();
    assert_eq!(completed.len(), 2);
    let (_, log) = verify(&completed[1].1, HEAD).unwrap();
    let basis = Basis {
        why: vec![Trigger::Asked],
        context: w.context(),
    };
    let provenance = Provenance {
        by: answer("judge-2", Judgment::Pass).by,
        execution: answer("judge-2", Judgment::Pass).execution.unwrap(),
    };
    assert_eq!(
        log.unwrap(),
        flow::receipt_digest(&flow::judgment_receipt_by(
            &w.revision(),
            2,
            Some(Judgment::Pass),
            Some(&provenance),
            Some(&basis),
        ))
    );
}

#[test]
fn a_stale_head_base_description_or_discussion_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    // Before any work: another head, another description, another
    // discussion than the request names.
    for change in [
        (|p: &mut Pr| p.head = "9".repeat(40)) as fn(&mut Pr),
        |p: &mut Pr| p.body = "It does more.".into(),
        |p: &mut Pr| p.title = "Fix it all".into(),
        |p: &mut Pr| {
            p.threads.get_mut("T1").unwrap().comments[1].body = "Edited.".into();
        },
        |p: &mut Pr| {
            p.reviews
                .push(json!({"id": 10, "state": "COMMENTED", "body": "Another.",
                "user": {"login": "reviewer", "type": "Bot"}}))
        },
    ] {
        let w = world();
        let request = judge_request(&w, vec![Trigger::Asked]);
        w.with(change);
        let before = w.posted().len();
        let judges = Scripted::answering(vec![answer("judge-1", Judgment::Pass)]);
        let p = producer(dir.path(), &w, judges.clone());
        assert!(ask(dir.path(), &p, &request).is_err());
        assert!(judges.0.lock().unwrap().asked.is_empty());
        assert_eq!(w.posted().len(), before);
    }
    // While it's judged: a retarget, a description edit or an answer
    // posted publishes nothing.
    for change in [
        (|p: &mut Pr| p.base = "release".into()) as fn(&mut Pr),
        |p: &mut Pr| p.body = "It does more.".into(),
        |p: &mut Pr| {
            p.threads
                .get_mut("T1")
                .unwrap()
                .comments
                .push(ThreadComment {
                    author: Some(FLOW.into()),
                    body: "Another answer.".into(),
                })
        },
    ] {
        let w = world();
        let request = judge_request(&w, vec![Trigger::Asked]);
        let judges = Scripted::answering(vec![answer("judge-1", Judgment::Pass)]);
        let during = w.clone();
        judges.0.lock().unwrap().during = Some(Box::new(move || during.with(change)));
        let p = producer(dir.path(), &w, judges);
        let refused = ask(dir.path(), &p, &request).unwrap_err();
        assert!(
            refused.contains("while it was judged") || refused.contains("asked on"),
            "{refused}"
        );
        assert!(w.posted().is_empty());
        assert!(w.with(|p| p.runs.is_empty()));
    }
}

#[test]
fn a_flow_that_wouldnt_count_the_judgment_is_refused_before_any_work() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let judges = Scripted::default();
    // A flow under another key, inline or by another file, under its key
    // both ways (which Conary won't load), or under none.
    for key in [
        json!({"judge_key": "cd".repeat(32)}),
        json!({"judge_key_file": "/run/elsewhere/judge-seal"}),
        json!({"judge_key": SEAL, "judge_key_file": FLOW_KEY}),
        json!({}),
    ] {
        let p = Producer::new(
            config_with(dir.path(), key.clone()),
            read(&w),
            github(&w),
            Box::new(judges.clone()),
            Signer::for_tests(PRODUCER, SECRET),
            seal(),
        )
        .unwrap();
        let refused = ask(dir.path(), &p, &judge_request(&w, vec![Trigger::Asked])).unwrap_err();
        assert!(refused.contains("seal key"), "{key}: {refused}");
    }
    // Nor an old contract's request, a resumed session, or no reason.
    let p = producer(dir.path(), &w, judges.clone());
    let mut old = judge_request(&w, vec![Trigger::Asked]);
    old.contract = "conary-plugin v3".into();
    let mut resumed = judge_request(&w, vec![Trigger::Asked]);
    resumed.execution.as_mut().unwrap().context = Context::Continue {
        session: "s".into(),
    };
    let unexplained = judge_request(&w, vec![]);
    let twice = judge_request(&w, vec![Trigger::Asked, Trigger::Asked]);
    for request in [old, resumed, unexplained, twice] {
        assert!(ask(dir.path(), &p, &request).is_err());
    }
    assert!(judges.0.lock().unwrap().asked.is_empty());
    assert!(w.with(|p| p.runs.is_empty()));
}

#[test]
fn a_flow_given_the_producers_own_key_inline_is_judged_for() {
    // The file form, the module's, is every other test's config.
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let judges = Scripted::answering(vec![answer("judge-1", Judgment::Pass)]);
    let p = Producer::new(
        config_with(dir.path(), json!({"judge_key": SEAL})),
        read(&w),
        github(&w),
        Box::new(judges.clone()),
        Signer::for_tests(PRODUCER, SECRET),
        seal(),
    )
    .unwrap();
    // Straight to the producer: the plugin's own config would rewrite the
    // flow's to the file form.
    ask(dir.path(), &p, &judge_request(&w, vec![Trigger::Asked])).unwrap();
    assert_eq!(judges.0.lock().unwrap().asked.len(), 1);
}

#[test]
fn generations_never_repeat_across_refusals_restarts_and_others_issuances() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    // A verdict account already posted generation 6 here.
    w.with(|p| {
        p.comments
            .push(json!({"user": {"login": FLOW, "type": "Bot"},
            "body": "old\n\n<!-- autoland-judgment:v2 {\"issued\":{\"generation\":6}} -->"}))
    });
    let request = |w: &World| judge_request(w, vec![Trigger::Asked]);
    let judges = Scripted::answering(vec![
        answer(AUTHOR, Judgment::Pass),
        answer("judge-1", Judgment::Fail),
    ]);
    let p = producer(dir.path(), &w, judges);
    // A refused judgment burns its generation, 7.
    assert!(ask(dir.path(), &p, &request(&w)).is_err());
    ask(dir.path(), &p, &request(&w)).unwrap();
    assert_eq!(generations(&w), 8);
    drop(p);
    // A restart carries on past it.
    let judges = Scripted::answering(vec![answer("judge-2", Judgment::Pass)]);
    let p = producer(dir.path(), &w, judges);
    ask(dir.path(), &p, &request(&w)).unwrap();
    assert_eq!(generations(&w), 9);
    // An account that isn't the flow's can't move them.
    w.with(|p| {
        p.comments
            .push(json!({"user": {"login": "someone", "type": "User"},
            "body": "<!-- autoland-judgment:v3 {\"issued\":{\"generation\":99}} -->"}))
    });
    assert_eq!(generations(&w), 9);
}

#[test]
fn a_restart_between_the_comment_and_the_check_run_publishes_the_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let judgment = Kept {
        repo: REPO.into(),
        pr: PR,
        revision: w.revision(),
        tree: TREE.into(),
        generation: 3,
        verdict: Some(Judgment::Owner),
        provenance: Some(Provenance {
            by: answer("judge-1", Judgment::Owner).by,
            execution: answer("judge-1", Judgment::Owner).execution.unwrap(),
        }),
        basis: Some(Basis {
            why: vec![Trigger::NoCi],
            context: w.context(),
        }),
        reasons: "It drops a table: <!-- not a marker --> Sam's call.".into(),
    };
    let body = comment(&seal(), &judgment).unwrap();
    assert!(!body.contains("<!-- not"), "{body}");
    let records =
        burrow_producer::records::Records::open(&dir.path().join("state/records")).unwrap();
    let target = burrow_producer::records::Target {
        repo: REPO.into(),
        head: HEAD.into(),
        check: CHECK.into(),
    };
    // One committed with its comment out, one committed before it went out,
    // and one never committed.
    let mut out = records.begin(target.clone()).unwrap();
    records.keep(&mut out, judgment.clone()).unwrap();
    records.commit(&mut out).unwrap();
    w.with(|p| {
        p.comments
            .push(json!({"user": {"login": FLOW, "type": "Bot"}, "body": body}))
    });
    let second = Kept {
        generation: 4,
        ..judgment.clone()
    };
    let mut held = records.begin(target.clone()).unwrap();
    records.keep(&mut held, second.clone()).unwrap();
    records.commit(&mut held).unwrap();
    let mut never = records.begin(target).unwrap();
    records
        .keep(
            &mut never,
            Kept {
                generation: 5,
                ..judgment.clone()
            },
        )
        .unwrap();

    let p = producer(dir.path(), &w, Scripted::default());
    assert_eq!(p.reconcile().unwrap(), 3);
    // Each committed one's comment once, the missing one posted now; and
    // each one's run once, the uncommitted one withdrawn.
    assert_eq!(
        w.posted(),
        [body.clone(), comment(&seal(), &second).unwrap()]
    );
    let mut conclusions: Vec<String> = w.completed().into_iter().map(|c| c.0).collect();
    conclusions.sort();
    assert_eq!(conclusions, ["success", "success"]);
    // A second start finds nothing left to do.
    let p = producer(dir.path(), &w, Scripted::default());
    assert_eq!(p.reconcile().unwrap(), 0);
    assert_eq!(w.posted().len(), 2);
    assert_eq!(w.completed().len(), 2);
}

#[test]
fn a_brief_shows_every_thread_and_answer_whatever_the_flow_asked_for() {
    let w = world();
    let discussion = Discussion::read(&w, REPO, PR).unwrap();
    let brief_with = |why: &[Trigger], ci: &brief::Ci| {
        brief::task(&brief::Judge {
            repo: REPO,
            pr: PR,
            head: HEAD,
            base: "main",
            title: "Fix it",
            body: "It fixes it.",
            why,
            owner: "Sam",
            discussion: &discussion,
            dir: Path::new("/looks/1"),
            ci,
        })
    };
    let brief = |why: &[Trigger]| brief_with(why, &brief::Ci::Green);
    for why in [
        vec![Trigger::Asked],
        vec![Trigger::R2],
        vec![Trigger::MajorDependency],
        vec![Trigger::RoundLimit],
        vec![Trigger::Changed, Trigger::AuthorEngine],
    ] {
        let text = brief(&why);
        for said in [
            "> P2: the loop never ends",
            "> It ends on the empty line, by design.",
            "> One finding.",
            "> It fixes it.",
        ] {
            assert!(text.contains(said), "{why:?} leaves out {said:?}");
        }
        assert!(text.contains(burrow_brief::FAILING));
        assert!(text.contains(burrow_brief::MUST_FIX));
        assert!(text.contains(&burrow_brief::land_or_owner("Sam")));
        // Where the producer laid out the head, its base and the diff.
        for laid in ["/looks/1/head", "/looks/1/base", "/looks/1/changes.diff"] {
            assert!(text.contains(laid), "{why:?} leaves out {laid}");
        }
    }
    assert!(brief(&[Trigger::RoundLimit]).contains(burrow_brief::PAST_LIMIT));
    assert!(brief(&[Trigger::MajorDependency]).contains(burrow_brief::DEP_SEEN));
    assert!(brief(&[Trigger::Asked]).contains(burrow_brief::REVIEWED));
    assert!(brief(&[Trigger::Asked]).contains("CI is green"));
    // Green on the flow's word alone: what the producer read or built
    // otherwise.
    let reported = brief_with(
        &[Trigger::Started],
        &brief::Ci::Reported(vec!["flake: failure".into()]),
    );
    assert!(!reported.contains("CI is green"), "{reported}");
    assert!(reported.contains("check it yourself"), "{reported}");
    assert!(reported.contains("> flake: failure"), "{reported}");
    let built = brief_with(&[Trigger::NoCi], &brief::Ci::Built(vec![]));
    assert!(!built.contains("CI is green"), "{built}");
    assert!(built.contains("No CI reported on its head"), "{built}");
}

#[test]
fn the_looks_context_is_conarys_own_reading() {
    let w = world();
    assert_eq!(
        w.context(),
        flow::read_judging_context(&w, REPO, PR).unwrap()
    );
}

#[test]
fn an_abandoned_judgment_publishes_nothing_and_starts_no_run() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let (hang_up, told) = std::sync::mpsc::channel();
    let judges = Scripted::answering(vec![answer("judge-1", Judgment::Pass)]);
    judges.0.lock().unwrap().during = Some(Box::new(move || hang_up.send(()).unwrap()));
    judges.0.lock().unwrap().wait_gone = true;
    let p = producer(dir.path(), &w, judges);
    let request = judge_request(&w, vec![Trigger::Asked]);
    assert!(
        ask_with(
            dir.path(),
            &|r, k: &Work| p.handle(r, k),
            &request,
            Some(told)
        )
        .is_err()
    );
    assert!(w.posted().is_empty());
    assert!(w.with(|p| p.runs.is_empty()));
}
