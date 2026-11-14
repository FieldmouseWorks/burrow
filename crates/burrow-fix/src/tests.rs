//! The broker against a scripted job runner and a scripted GitHub, through
//! the plugin and the kit's socket, as the flow reaches it.
use super::*;
use crate::broker::{Broker, GitHub, key};
use crate::jobs::{Jobs, Record, Turn};
use burrow_github::{Api, Failed, Response as GhResponse};
use burrow_producer::serve::{Controller, Server};
use conary_core::plugin::{Request, fixtures};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[test]
fn the_plugin_offers_what_the_fixing_roles_engine_offers() {
    let dir = tempfile::tempdir().unwrap();
    let p = plugin(&config(dir.path())).unwrap();
    assert_eq!(p.by.engine, "claude");
    let Answer::Describe { models, kinds, .. } = &p.describe else {
        panic!("{:?}", p.describe)
    };
    assert_eq!(kinds, &[Kind::Fix]);
    assert_eq!(
        models
            .iter()
            .map(|m| (m.id.as_str(), m.efforts.clone()))
            .collect::<Vec<_>>(),
        [("a-model", vec!["high".to_owned()])]
    );
}

const REPO: &str = "sam/box";
const HEAD: &str = "1111111111111111111111111111111111111111";
const PUSHED: &str = "2222222222222222222222222222222222222222";
const OTHER: &str = "3333333333333333333333333333333333333333";

/// A job runner that runs nothing: each job's record is what the test
/// scripts, and what it was asked is kept.
#[derive(Default)]
struct Runner {
    started: Vec<(String, String, String)>,
    /// The records `why` answers in order; the last one stays.
    records: VecDeque<Record>,
    stopped: Vec<String>,
    answers: Option<Value>,
    /// Keys proj still holds a job under.
    held: Vec<(String, String)>,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Runner>>);

impl Jobs for Fake {
    fn start(&self, repo: &str, key: &str, task: &str) -> Result<String, String> {
        let mut r = self.0.lock().unwrap();
        let id = format!("job-{}", r.started.len() + 1);
        r.started.push((repo.into(), key.into(), task.into()));
        r.held.push((key.into(), id.clone()));
        Ok(id)
    }
    fn find(&self, key: &str) -> Result<Option<String>, String> {
        let r = self.0.lock().unwrap();
        Ok(r.held
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, id)| id.clone()))
    }
    fn why(&self, _: &str) -> Result<Record, String> {
        let mut r = self.0.lock().unwrap();
        let next = if r.records.len() > 1 {
            r.records.pop_front()
        } else {
            r.records.front().cloned()
        };
        next.ok_or_else(|| "no record".into())
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        self.0.lock().unwrap().stopped.push(id.into());
        Ok(())
    }
    fn answers(&self, _: &str) -> Result<Option<Vec<u8>>, String> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .answers
            .as_ref()
            .map(|a| a.to_string().into_bytes()))
    }
}

/// A GitHub whose PR has the heads scripted, one a read; the last stays.
#[derive(Clone)]
struct Pr(Arc<Mutex<VecDeque<String>>>);

impl Api for Pr {
    fn request(
        &mut self,
        _: &str,
        path: &str,
        _: Option<&Value>,
        _: Option<&str>,
    ) -> Result<GhResponse, Failed> {
        assert_eq!(path, format!("repos/{REPO}/pulls/7"));
        let mut heads = self.0.lock().unwrap();
        let head = if heads.len() > 1 {
            heads.pop_front().unwrap()
        } else {
            heads.front().cloned().unwrap()
        };
        let body = json!({"state": "open",
            "head": {"sha": head, "ref": "topic", "repo": {"full_name": "Sam/Box"}}});
        Ok(GhResponse {
            status: 200,
            headers: Default::default(),
            body: body.to_string(),
        })
    }
}

fn record(stage: &str) -> Record {
    Record {
        stage: stage.into(),
        overdue_at: None,
        ends: if stage == "done" {
            vec!["done".into()]
        } else {
            vec![]
        },
        turn: Some(Turn {
            engine: Some("claude".into()),
            model: Some("a-model".into()),
            effort: Some("high".into()),
            session: Some("s-1".into()),
        }),
    }
}

/// Settings that route every role to claude, which offers one model at
/// one effort.
fn settings(dir: &std::path::Path) -> burrow_settings::Config {
    let overlay = dir.join("overlay.json");
    std::fs::write(
        overlay.with_extension("roles.json"),
        json!({"offers": {"claude": {"models": ["a-model"], "efforts": ["high"]}}, "migrated": 1})
            .to_string(),
    )
    .unwrap();
    serde_json::from_value(json!({"overlay": overlay})).unwrap()
}

fn config(dir: &std::path::Path) -> Config {
    Config {
        socket: dir.join("fix.sock"),
        controller: "flow".into(),
        settings: settings(dir),
        proj: vec!["proj".into()],
        jobs: dir.join("jobs"),
        role: "fix".into(),
        app: None,
        timeout: "60m".into(),
        idle: "20m".into(),
        gh: "gh".into(),
        state: dir.join("state"),
    }
}

struct Rig {
    dir: tempfile::TempDir,
    cfg: Config,
    runner: Fake,
    heads: Arc<Mutex<VecDeque<String>>>,
    broker: Arc<Broker>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let runner = Fake::default();
    let heads = Arc::new(Mutex::new(VecDeque::from([HEAD.to_owned()])));
    let h = heads.clone();
    let github: GitHub = Box::new(move || Box::new(Pr(h.clone())));
    let mut broker = Broker::new(cfg.clone(), Box::new(runner.clone()), github).unwrap();
    broker.look = Duration::from_millis(20);
    Rig {
        dir,
        cfg,
        runner,
        heads,
        broker: Arc::new(broker),
    }
}

impl Rig {
    /// Serve `n` connections in the background.
    fn serve(&self, n: usize) -> thread::JoinHandle<()> {
        let me = Controller(rustix::process::getuid().as_raw());
        let server = Server::bind(&self.cfg.socket, me).unwrap();
        let broker = self.broker.clone();
        thread::spawn(move || {
            let handler = |r: Request, w: &burrow_producer::serve::Work| broker.handle(r, w);
            for _ in 0..n {
                server.once(&handler).unwrap();
            }
        })
    }

    /// Serve every connection, each on its own thread, for the rest of the
    /// test.
    fn serve_all(&self) {
        let me = Controller(rustix::process::getuid().as_raw());
        let server = Server::bind(&self.cfg.socket, me).unwrap();
        let broker = self.broker.clone();
        thread::spawn(move || {
            server.serve(move |r: Request, w: &burrow_producer::serve::Work| broker.handle(r, w))
        });
    }

    fn ask(&self, request: &Value) -> Result<Value, String> {
        let mut out = Vec::new();
        plugin(&self.cfg)?.answer(&mut request.to_string().as_bytes(), &mut out)?;
        Ok(serde_json::from_slice(&out).unwrap())
    }

    fn script(&self, records: &[Record], answers: Value, heads: &[&str]) {
        let mut r = self.runner.0.lock().unwrap();
        r.records = records.iter().cloned().collect();
        r.answers = Some(answers);
        *self.heads.lock().unwrap() = heads.iter().map(|h| (*h).to_owned()).collect();
    }
}

fn problems() -> Value {
    json!([
        {"kind": "finding", "finding": {"path": "src/lib.rs", "line": 3, "severity": "p1", "text": "it panics on empty input"}},
        {"kind": "red_check", "check": "tests", "text": "one failed"},
        {"kind": "finding", "finding": {"path": "src/lib.rs", "line": 9, "severity": "p2", "text": "a nit"}},
    ])
}

fn fix(head: &str) -> Value {
    json!({"contract": conary_core::plugin::CONTRACT, "kind": "fix", "repo": REPO,
        "pr": 7, "head": head, "problems": problems(),
        "execution": {"choice": null, "context": {"kind": "fresh"}}})
}

fn answered(pushed: Option<&str>) -> Value {
    json!({"pushed": pushed, "unable": null, "answers": [
        {"problem": 0, "fixed": pushed.is_some(), "reply": "guarded the empty case"},
        {"problem": 2, "fixed": false, "reply": "a style preference no rule asks for"},
    ]})
}

#[test]
fn the_fixtures_are_answered_unfixed_with_no_job() {
    for f in fixtures() {
        let Some(Answer::Fix { result, answers }) = fixture(&f.task) else {
            continue;
        };
        assert!(f.conforms(&Answer::Fix {
            result,
            answers: answers.clone()
        }));
        assert!(answers.unwrap().iter().all(|a| !a.fixed));
    }
}

#[test]
fn a_fix_runs_as_one_job_and_answers_what_github_shows_it_pushed() {
    let r = rig();
    r.script(
        &[record("queued"), record("running"), record("done")],
        answered(Some(PUSHED)),
        &[HEAD, PUSHED],
    );
    let t = r.serve(1);
    let got = r.ask(&fix(HEAD)).unwrap();
    t.join().unwrap();
    assert_eq!(got["result"], json!({"status": "pushed", "head": PUSHED}));
    assert_eq!(got["answers"][0]["problem"], 0);
    assert_eq!(got["answers"][1]["problem"], 2);
    assert_eq!(got["by"], json!({"engine": "claude", "session": "s-1"}));
    assert_eq!(got["execution"]["model"], "a-model");
    let runner = r.runner.0.lock().unwrap();
    let (repo, k, task) = &runner.started[0];
    assert_eq!((repo.as_str(), k.as_str()), (REPO, key(REPO, 7).as_str()));
    assert!(task.contains("0. A review finding") && task.contains("1. The check tests is red"));
    assert!(task.contains(burrow_brief::FINDINGS.trim_end()));
    assert!(runner.stopped.is_empty(), "a done job needs no stopping");
    // Its key is let go: nothing is kept for a restart to stop.
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
}

fn fs_count(dir: &std::path::Path) -> usize {
    std::fs::read_dir(dir).unwrap().count()
}

#[test]
fn a_request_for_a_head_that_isnt_the_prs_starts_nothing() {
    let r = rig();
    r.script(&[record("done")], answered(None), &[OTHER]);
    let t = r.serve(1);
    let err = r.ask(&fix(HEAD)).unwrap_err();
    t.join().unwrap();
    assert!(err.contains(&format!("head is {OTHER}")), "{err}");
    assert!(r.runner.0.lock().unwrap().started.is_empty());
}

#[test]
fn a_reported_head_that_isnt_the_prs_is_refused() {
    let r = rig();
    // The job says it pushed, and GitHub has another head.
    r.script(&[record("done")], answered(Some(PUSHED)), &[HEAD, OTHER]);
    let t = r.serve(2);
    let err = r.ask(&fix(HEAD)).unwrap_err();
    assert!(err.contains(&format!("says it pushed {PUSHED}")), "{err}");
    // Or says it pushed nothing, and the head moved.
    r.script(&[record("done")], answered(None), &[HEAD, OTHER]);
    let err = r.ask(&fix(HEAD)).unwrap_err();
    t.join().unwrap();
    assert!(
        err.contains("pushed nothing, and the PR's head moved"),
        "{err}"
    );
}

#[test]
fn answers_that_dont_match_the_problems_are_refused() {
    let one = |problem: usize, fixed: bool, reply: &str| json!({"problem": problem, "fixed": fixed, "reply": reply});
    for (said, why) in [
        // One missing, one extra, reordered, one for a red check.
        (json!([one(0, false, "no")]), "not the findings"),
        (
            json!([one(0, false, "a"), one(2, false, "b"), one(2, false, "c")]),
            "not the findings",
        ),
        (
            json!([one(2, false, "a"), one(0, false, "b")]),
            "not the findings",
        ),
        (
            json!([one(0, false, "a"), one(1, false, "b")]),
            "not the findings",
        ),
        (
            json!([one(0, false, " "), one(2, false, "b")]),
            "has no reply",
        ),
        // Fixed with nothing pushed.
        (
            json!([one(0, true, "a"), one(2, false, "b")]),
            "pushed nothing",
        ),
    ] {
        let r = rig();
        let answers = json!({"pushed": null, "unable": null, "answers": said});
        r.script(&[record("done")], answers, &[HEAD]);
        let t = r.serve(1);
        let err = r.ask(&fix(HEAD)).unwrap_err();
        t.join().unwrap();
        assert!(err.contains(why), "{said}: {err}");
    }
}

#[test]
fn a_second_concurrent_fix_of_one_pr_is_refused() {
    let r = rig();
    r.script(&[record("running")], answered(None), &[HEAD]);
    // Connections served at once, as the broker serves them.
    r.serve_all();
    let first = {
        let (cfg, request) = (r.cfg.clone(), fix(HEAD));
        thread::spawn(move || {
            let mut out = Vec::new();
            plugin(&cfg)?.answer(&mut request.to_string().as_bytes(), &mut out)
        })
    };
    while r.runner.0.lock().unwrap().started.is_empty() {
        thread::sleep(Duration::from_millis(10));
    }
    // The second: refused at once, or (were it let through) a second job.
    let second = {
        let (cfg, request) = (r.cfg.clone(), fix(HEAD));
        thread::spawn(move || {
            let mut out = Vec::new();
            plugin(&cfg)?.answer(&mut request.to_string().as_bytes(), &mut out)
        })
    };
    while !second.is_finished() && r.runner.0.lock().unwrap().started.len() < 2 {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        r.runner.0.lock().unwrap().started.len(),
        1,
        "a second job started"
    );
    let err = second.join().unwrap().unwrap_err();
    assert!(err.contains("at work already"), "{err}");
    // The first finishes, and its answer goes back.
    r.script(&[record("done")], answered(None), &[HEAD]);
    first.join().unwrap().unwrap();
    // And with it over, the PR may be fixed again.
    r.script(&[record("done")], answered(None), &[HEAD]);
    r.ask(&fix(HEAD)).unwrap();
    assert_eq!(r.runner.0.lock().unwrap().started.len(), 2);
}

#[test]
fn a_cancelled_request_leaves_no_running_job() {
    use std::io::Write as _;
    let r = rig();
    r.script(&[record("running")], answered(None), &[HEAD]);
    let t = r.serve(1);
    let mut stream = std::os::unix::net::UnixStream::connect(&r.cfg.socket).unwrap();
    stream
        .write_all(format!("{}\n", fix(HEAD)).as_bytes())
        .unwrap();
    while r.runner.0.lock().unwrap().started.is_empty() {
        thread::sleep(Duration::from_millis(10));
    }
    // The plugin is killed: its connection ends.
    drop(stream);
    t.join().unwrap();
    assert_eq!(r.runner.0.lock().unwrap().stopped, ["job-1"]);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
}

#[test]
fn an_overdue_job_or_one_that_ends_otherwise_is_stopped_and_answers_nothing() {
    let mut overdue = record("running");
    overdue.overdue_at = Some(1);
    let mut asked = record("ask");
    asked.ends = vec!["ask".into()];
    for (rec, why) in [(overdue, "is overdue"), (asked, "ended ask, not done")] {
        let r = rig();
        r.script(&[rec], answered(None), &[HEAD]);
        let t = r.serve(1);
        let err = r.ask(&fix(HEAD)).unwrap_err();
        t.join().unwrap();
        assert!(err.contains(why), "{err}");
        assert_eq!(r.runner.0.lock().unwrap().stopped, ["job-1"]);
    }
}

#[test]
fn a_restart_stops_the_jobs_whose_requests_it_lost() {
    let r = rig();
    let k = key(REPO, 7);
    std::fs::write(r.cfg.state.join("live").join(k.replace('/', "+")), &k).unwrap();
    r.runner.0.lock().unwrap().held.push((k, "job-9".into()));
    assert_eq!(r.broker.reconcile(), Ok(1));
    assert_eq!(r.runner.0.lock().unwrap().stopped, ["job-9"]);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
    let _ = &r.dir;
}

#[test]
fn a_prs_key_is_its_own() {
    assert_eq!(key("Sam/My_Box", 7), "fix/sam/my:box/7");
    assert_ne!(key("sam/my_box", 7), key("sam/my-box", 7));
    assert_ne!(key("sam/my_box", 7), key("sam/my.box", 7));
}
