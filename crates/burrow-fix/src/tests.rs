//! The broker against a scripted job runner and a scripted GitHub, through
//! the plugin and the kit's socket, as the flow reaches it.
use super::*;
use crate::broker::{Broker, GitHub, key};
use burrow_github::{Api, Failed, Response as GhResponse};
use burrow_producer::jobs::{Jobs, Record, Turn};
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

/// The choice the flow's configuration is rendered with (its `execution`)
/// is the fixing role's, with no repo's own, since conaryd reads one
/// configuration for every repo: exactly what a job in a repo the role isn't
/// set apart for is held to, and refused in one it is. One the engine
/// doesn't offer is refused, as a job on it would be.
#[test]
fn the_flows_choice_is_the_fixing_roles() {
    use burrow_producer::jobs::{choice, offered, runnable};
    let dir = tempfile::tempdir().unwrap();
    let cfg = route(
        dir.path(),
        json!({"models": {
            "fix claude": {"model": "a-model", "effort": "high"},
            "fix@sam/other claude": {"model": "a-model"},
        }}),
    );
    let chosen = choice(&cfg, "fix").unwrap();
    assert_eq!(
        (
            chosen.engine.as_str(),
            chosen.model.as_deref(),
            chosen.effort.as_deref()
        ),
        ("claude", Some("a-model"), Some("high"))
    );
    let offer = offered(&cfg, "fix").unwrap();
    runnable(&cfg, "fix", REPO, &offer, Some(&chosen)).unwrap();
    let err = runnable(&cfg, "fix", "sam/other", &offer, Some(&chosen)).unwrap_err();
    assert!(err.contains("the flow chose"), "{err}");

    let dir = tempfile::tempdir().unwrap();
    let unoffered = route(
        dir.path(),
        json!({"models": {"fix claude": {"model": "b-model"}}}),
    );
    let err = choice(&unoffered, "fix").unwrap_err();
    assert!(err.contains("doesn't offer that model"), "{err}");
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
    /// Keys proj still holds a job under: never let go of, as proj never
    /// lets go of a key once its job ran a turn.
    held: Vec<(String, String)>,
    /// Whether a stop fails.
    stop_fails: bool,
    /// Whether a start reads as failed, though its job started.
    start_fails: bool,
}

#[derive(Clone, Default)]
struct Fake(Arc<Mutex<Runner>>);

impl Jobs for Fake {
    fn start(&self, repo: &str, key: &str, task: &str) -> Result<String, String> {
        let mut r = self.0.lock().unwrap();
        if r.held.iter().any(|(k, _)| k == key) {
            return Err(format!("{key} is held"));
        }
        let id = format!("job-{}", r.started.len() + 1);
        r.started.push((repo.into(), key.into(), task.into()));
        r.held.push((key.into(), id.clone()));
        if r.start_fails {
            return Err("proj run failed".into());
        }
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
        let mut r = self.0.lock().unwrap();
        if r.stop_fails {
            return Err("proj stop failed".into());
        }
        r.stopped.push(id.into());
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

/// A GitHub whose PR has the heads scripted, one a read (the last stays),
/// and whose compare of two commits says what's scripted.
#[derive(Clone)]
struct Pr {
    heads: Arc<Mutex<VecDeque<String>>>,
    compare: Arc<Mutex<String>>,
}

impl Api for Pr {
    fn request(
        &mut self,
        _: &str,
        path: &str,
        _: Option<&Value>,
        _: Option<&str>,
    ) -> Result<GhResponse, Failed> {
        let body = if path.starts_with(&format!("repos/{REPO}/compare/{HEAD}...")) {
            json!({"status": *self.compare.lock().unwrap()})
        } else {
            assert_eq!(path, format!("repos/{REPO}/pulls/7"));
            self.pr()
        };
        Ok(GhResponse {
            status: 200,
            headers: Default::default(),
            body: body.to_string(),
        })
    }
}

impl Pr {
    fn pr(&self) -> Value {
        let mut heads = self.heads.lock().unwrap();
        let head = if heads.len() > 1 {
            heads.pop_front().unwrap()
        } else {
            heads.front().cloned().unwrap()
        };
        json!({"state": "open",
            "head": {"sha": head, "ref": "topic", "repo": {"full_name": "Sam/Box"}}})
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
/// one effort, and run the fixing role on that model and effort.
fn settings(dir: &std::path::Path) -> burrow_settings::Config {
    route(
        dir,
        json!({"models": {"fix claude": {"model": "a-model", "effort": "high"}}}),
    )
}

/// Settings with `routing` over the schema's defaults.
fn route(dir: &std::path::Path, routing: Value) -> burrow_settings::Config {
    let overlay = dir.join("overlay.json");
    std::fs::write(
        overlay.with_extension("roles.json"),
        json!({"routing": routing,
            "offers": {"claude": {"models": ["a-model"], "efforts": ["high"]}}, "migrated": 1})
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
        stuck: None,
        flow: None,
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
    compare: Arc<Mutex<String>>,
    broker: Arc<Broker>,
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    let runner = Fake::default();
    let heads = Arc::new(Mutex::new(VecDeque::from([HEAD.to_owned()])));
    let compare = Arc::new(Mutex::new("ahead".to_owned()));
    let pr = Pr {
        heads: heads.clone(),
        compare: compare.clone(),
    };
    let github: GitHub = Box::new(move || Box::new(pr.clone()));
    let mut broker = Broker::new(cfg.clone(), Box::new(runner.clone()), github).unwrap();
    broker.look = Duration::from_millis(20);
    Rig {
        dir,
        cfg,
        runner,
        heads,
        compare,
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
        "execution": {"choice": null, "context": {"kind": "fresh"}},
        "negotiated": negotiated()})
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
    assert_eq!(repo, REPO);
    assert!(k.starts_with(&format!("{}/", key(REPO, 7))), "{k}");
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
fn a_prs_key_is_its_own() {
    assert_eq!(key("Sam/My_Box", 7), "fix/sam/my:box/7");
    assert_ne!(key("sam/my_box", 7), key("sam/my-box", 7));
    assert_ne!(key("sam/my_box", 7), key("sam/my.box", 7));
}

#[test]
fn a_push_that_isnt_a_new_commit_on_the_handed_head_is_refused() {
    // The job names the head it was handed, or one that replaced it: the
    // PR's head is what it says, and GitHub's compare says it's no new
    // commit on top.
    for (said, status) in [
        (HEAD, "identical"),
        (PUSHED, "diverged"),
        (PUSHED, "behind"),
    ] {
        let r = rig();
        r.script(&[record("done")], answered(Some(said)), &[HEAD, said]);
        *r.compare.lock().unwrap() = status.into();
        let t = r.serve(1);
        let err = r.ask(&fix(HEAD)).unwrap_err();
        t.join().unwrap();
        assert!(err.contains("isn't a new commit"), "{said} {status}: {err}");
    }
}

#[test]
fn a_fix_the_fixing_role_wouldnt_run_as_asked_starts_nothing() {
    let choice = |model: &str, effort: &str| {
        let mut f = fix(HEAD);
        f["execution"]["choice"] = json!({"engine": "claude", "model": model, "effort": effort});
        f
    };
    let chose = |choice: Value| {
        let mut f = fix(HEAD);
        f["execution"]["choice"] = choice;
        f
    };
    let fine = json!({"models": {"fix claude": {"model": "a-model", "effort": "high"}}});
    for (routing, request, why) in [
        // The flow chose another model, or another effort.
        (fine.clone(), choice("b-model", "high"), "the flow chose"),
        (fine.clone(), choice("a-model", "low"), "the flow chose"),
        // The flow chose the model alone, or the engine alone, and the role
        // would run at an effort, or on a model: what ran would be refused.
        (
            fine.clone(),
            chose(json!({"engine": "claude", "model": "a-model"})),
            "the flow chose",
        ),
        (
            fine.clone(),
            chose(json!({"engine": "claude"})),
            "the flow chose",
        ),
        // The role names no model, so what ran couldn't be said.
        (json!({}), fix(HEAD), "names no model"),
    ] {
        let mut r = rig();
        r.cfg.settings = route(r.dir.path(), routing);
        let mut broker = Broker::new(
            r.cfg.clone(),
            Box::new(r.runner.clone()),
            Box::new(|| unreachable!("nothing is read of GitHub")),
        )
        .unwrap();
        broker.look = Duration::from_millis(20);
        r.broker = Arc::new(broker);
        r.script(&[record("done")], answered(None), &[HEAD]);
        let t = r.serve(1);
        let err = r.ask(&request).unwrap_err();
        t.join().unwrap();
        assert!(err.contains(why), "{request}: {err}");
        assert!(r.runner.0.lock().unwrap().started.is_empty());
    }
}

#[test]
fn a_job_that_wont_stop_keeps_its_pr_held() {
    let mut overdue = record("running");
    overdue.overdue_at = Some(1);
    let r = rig();
    r.script(&[overdue], answered(None), &[HEAD]);
    r.runner.0.lock().unwrap().stop_fails = true;
    let t = r.serve(3);
    let err = r.ask(&fix(HEAD)).unwrap_err();
    assert!(err.contains("wouldn't stop"), "{err}");
    // No second fix starts beside it, and a restart can find it.
    let err = r.ask(&fix(HEAD)).unwrap_err();
    assert!(err.contains("at work already"), "{err}");
    assert_eq!(r.runner.0.lock().unwrap().started.len(), 1);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 1);
    // Once it stops, the PR's next fix runs.
    r.runner.0.lock().unwrap().stop_fails = false;
    r.script(&[record("done")], answered(None), &[HEAD]);
    r.ask(&fix(HEAD)).unwrap();
    t.join().unwrap();
    let runner = r.runner.0.lock().unwrap();
    assert_eq!(
        (runner.started.len(), &runner.stopped[..]),
        (2, &["job-1".to_owned()][..])
    );
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
}

#[test]
fn a_start_that_read_as_failed_leaves_no_job_at_work() {
    let r = rig();
    r.script(&[record("running")], answered(None), &[HEAD]);
    r.runner.0.lock().unwrap().start_fails = true;
    let t = r.serve(2);
    let err = r.ask(&fix(HEAD)).unwrap_err();
    assert!(err.contains("proj run failed"), "{err}");
    assert_eq!(r.runner.0.lock().unwrap().stopped, ["job-1"]);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
    r.runner.0.lock().unwrap().start_fails = false;
    r.script(&[record("done")], answered(None), &[HEAD]);
    r.ask(&fix(HEAD)).unwrap();
    t.join().unwrap();
}

/// The runner of a broker whose process is gone: once cut, a call never
/// returns, as a killed broker asks proj nothing more, and stops nothing on
/// its way out.
struct Inert {
    runner: Fake,
    cut: Arc<std::sync::atomic::AtomicBool>,
}

impl Inert {
    fn alive(&self) {
        while self.cut.load(std::sync::atomic::Ordering::SeqCst) {
            thread::park_timeout(Duration::from_secs(1));
        }
    }
}

impl Jobs for Inert {
    fn start(&self, repo: &str, key: &str, task: &str) -> Result<String, String> {
        self.alive();
        self.runner.start(repo, key, task)
    }
    fn find(&self, key: &str) -> Result<Option<String>, String> {
        self.alive();
        self.runner.find(key)
    }
    fn why(&self, id: &str) -> Result<Record, String> {
        self.alive();
        self.runner.why(id)
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        self.alive();
        self.runner.stop(id)
    }
    fn answers(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
        self.alive();
        self.runner.answers(id)
    }
}

impl Rig {
    /// A broker on this rig's state, runner and GitHub, as a restart
    /// builds it, on `jobs`.
    fn broker_on(&self, jobs: Box<dyn Jobs>) -> Broker {
        let pr = Pr {
            heads: self.heads.clone(),
            compare: self.compare.clone(),
        };
        let github: GitHub = Box::new(move || Box::new(pr.clone()));
        let mut broker = Broker::new(self.cfg.clone(), jobs, github).unwrap();
        broker.look = Duration::from_millis(20);
        broker
    }
}

/// A rig whose broker restarted while job-1, a fix of `HEAD` asked as
/// `fix(HEAD)` asks it, was at work: the old broker's process is gone, and
/// the flow's plugin saw its connection close.
fn restarted() -> Rig {
    use std::io::Write as _;
    let mut r = rig();
    r.script(&[record("running")], answered(None), &[HEAD]);
    let cut = Arc::new(std::sync::atomic::AtomicBool::new(false));
    r.broker = Arc::new(r.broker_on(Box::new(Inert {
        runner: r.runner.clone(),
        cut: cut.clone(),
    })));
    let _old = r.serve(1);
    let mut stream = std::os::unix::net::UnixStream::connect(&r.cfg.socket).unwrap();
    stream
        .write_all(format!("{}\n", fix(HEAD)).as_bytes())
        .unwrap();
    while r.runner.0.lock().unwrap().started.is_empty() {
        thread::sleep(Duration::from_millis(10));
    }
    cut.store(true, std::sync::atomic::Ordering::SeqCst);
    drop(stream);
    r.broker = Arc::new(r.broker_on(Box::new(r.runner.clone())));
    r.broker.reconcile().unwrap();
    r
}

#[test]
fn a_restart_leaves_its_fix_job_at_work_and_answers_the_remade_request_from_it() {
    let r = restarted();
    assert!(
        r.runner.0.lock().unwrap().stopped.is_empty(),
        "the restart stopped the fix job"
    );
    // The flow asks again for the same PR, kind and head: the job it
    // started before the restart answers it, with what it pushed meanwhile.
    r.script(
        &[record("running"), record("done")],
        answered(Some(PUSHED)),
        &[PUSHED],
    );
    let t = r.serve(1);
    let got = r.ask(&fix(HEAD)).unwrap();
    t.join().unwrap();
    assert_eq!(got["result"], json!({"status": "pushed", "head": PUSHED}));
    assert_eq!(got["answers"][0]["problem"], 0);
    let runner = r.runner.0.lock().unwrap();
    assert_eq!(runner.started.len(), 1, "a second fix job started");
    assert!(runner.stopped.is_empty(), "{:?}", runner.stopped);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
}

#[test]
fn a_restart_holds_a_lost_requests_pr_while_its_job_is_at_work() {
    let r = rig();
    let k = format!("{}/1", key(REPO, 7));
    // As an older broker kept it: the job's key alone, so no request can
    // say it asks the same.
    std::fs::write(r.cfg.state.join("live").join(k.replace('/', "+")), &k).unwrap();
    r.runner.0.lock().unwrap().held.push((k, "job-9".into()));
    assert_eq!(r.broker.reconcile(), Ok(1));
    assert!(r.runner.0.lock().unwrap().stopped.is_empty());
    assert_eq!(fs_count(&r.cfg.state.join("live")), 1);
    // A request is refused while that job is at work, and once it has
    // ended, the PR's next fix runs, with nothing stopped.
    r.script(
        &[record("running"), record("done"), record("done")],
        answered(None),
        &[HEAD],
    );
    let t = r.serve(2);
    let err = r.ask(&fix(HEAD)).unwrap_err();
    assert!(err.contains("at work already"), "{err}");
    r.ask(&fix(HEAD)).unwrap();
    t.join().unwrap();
    let runner = r.runner.0.lock().unwrap();
    assert_eq!(runner.started.len(), 1);
    assert!(runner.stopped.is_empty(), "{:?}", runner.stopped);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
    let _ = &r.dir;
}

#[test]
fn a_request_for_another_head_stops_the_adopted_job_and_starts_its_own() {
    let r = restarted();
    // The PR moved on to OTHER while the broker was down: the job handed
    // HEAD is at work on a head the flow has given up on.
    r.script(
        &[record("running"), record("running")],
        answered(None),
        &[OTHER],
    );
    r.serve_all();
    let asked = {
        let (cfg, request) = (r.cfg.clone(), fix(OTHER));
        thread::spawn(move || {
            let mut out = Vec::new();
            plugin(&cfg)?.answer(&mut request.to_string().as_bytes(), &mut out)
        })
    };
    while r.runner.0.lock().unwrap().started.len() < 2 && !asked.is_finished() {
        thread::sleep(Duration::from_millis(10));
    }
    {
        let runner = r.runner.0.lock().unwrap();
        assert_eq!(runner.stopped, ["job-1"], "the superseded job runs on");
        assert_eq!(runner.started.len(), 2, "{:?}", runner.started);
        let (_, k, task) = &runner.started[1];
        assert!(task.contains(OTHER), "{task}");
        // What's kept on disk is the new job's, not the superseded one's.
        let kept: Vec<String> = std::fs::read_dir(r.cfg.state.join("live"))
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect();
        assert_eq!(kept.len(), 1, "{kept:?}");
        assert_eq!(kept[0].lines().next(), Some(k.as_str()));
        assert_eq!(kept[0].lines().nth(1), Some(OTHER));
    }
    r.script(&[record("done")], answered(None), &[OTHER]);
    asked.join().unwrap().unwrap();
    assert_eq!(r.runner.0.lock().unwrap().started.len(), 2);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
}

#[test]
fn a_superseded_job_that_wont_stop_keeps_its_pr_held() {
    let r = restarted();
    r.script(&[record("running")], answered(None), &[OTHER]);
    r.runner.0.lock().unwrap().stop_fails = true;
    let t = r.serve(2);
    let err = r.ask(&fix(OTHER)).unwrap_err();
    assert!(err.contains("won't stop"), "{err}");
    assert_eq!(r.runner.0.lock().unwrap().started.len(), 1);
    // Kept, as any job that wouldn't stop: a restart still finds it so.
    let kept = std::fs::read_dir(r.cfg.state.join("live"))
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect::<Vec<_>>();
    assert!(kept.len() == 1 && kept[0].ends_with("kept\n"), "{kept:?}");
    // Once it stops, the PR's next fix runs.
    r.runner.0.lock().unwrap().stop_fails = false;
    r.script(&[record("done")], answered(None), &[OTHER]);
    r.ask(&fix(OTHER)).unwrap();
    t.join().unwrap();
    let runner = r.runner.0.lock().unwrap();
    assert_eq!(
        (runner.started.len(), &runner.stopped[..]),
        (2, &["job-1".to_owned()][..])
    );
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
}

#[test]
fn a_remade_request_with_other_problems_joins_nothing() {
    let r = restarted();
    let mut other = fix(HEAD);
    other["problems"].as_array_mut().unwrap().remove(1);
    // Its first look finds the job at work; a job started for it instead
    // ends at its second.
    let mut asked = record("ask");
    asked.ends = vec!["ask".into()];
    r.script(&[record("running"), asked], answered(None), &[HEAD]);
    let t = r.serve(1);
    let err = r.ask(&other).unwrap_err();
    t.join().unwrap();
    assert!(err.contains("at work already"), "{err}");
    let runner = r.runner.0.lock().unwrap();
    assert_eq!(runner.started.len(), 1);
    assert!(runner.stopped.is_empty(), "{:?}", runner.stopped);
}

#[test]
fn an_adopted_job_that_ended_holding_anything_open_is_stopped_before_a_fresh_fix() {
    let r = restarted();
    let mut asked = record("ask");
    asked.ends = vec!["ask".into()];
    // Asked of the same head, with other problems.
    let mut other = fix(HEAD);
    other["problems"].as_array_mut().unwrap().remove(1);
    r.script(
        &[asked, record("done")],
        json!({"pushed": null, "unable": null, "answers": [
        {"problem": 0, "fixed": false, "reply": "a"},
        {"problem": 1, "fixed": false, "reply": "b"}]}),
        &[HEAD],
    );
    let t = r.serve(1);
    r.ask(&other).unwrap();
    t.join().unwrap();
    let runner = r.runner.0.lock().unwrap();
    assert_eq!(runner.stopped, ["job-1"]);
    assert_eq!(runner.started.len(), 2);
}

#[test]
fn a_job_that_ended_while_the_broker_was_down_answers_the_remade_request() {
    let r = restarted();
    r.script(&[record("done")], answered(Some(PUSHED)), &[PUSHED]);
    let t = r.serve(1);
    let got = r.ask(&fix(HEAD)).unwrap();
    t.join().unwrap();
    assert_eq!(got["result"], json!({"status": "pushed", "head": PUSHED}));
    let runner = r.runner.0.lock().unwrap();
    assert_eq!(runner.started.len(), 1);
    assert!(runner.stopped.is_empty(), "{:?}", runner.stopped);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
}

#[test]
fn a_restart_still_stops_a_kept_job_before_the_prs_next_fix() {
    let r = rig();
    let k = format!("{}/1", key(REPO, 7));
    // A job whose request ended and that wouldn't stop.
    std::fs::write(
        r.cfg.state.join("live").join(k.replace('/', "+")),
        format!("{k}\n\n\nkept\n"),
    )
    .unwrap();
    {
        let mut runner = r.runner.0.lock().unwrap();
        runner.held.push((k, "job-9".into()));
        runner.stop_fails = true;
    }
    assert_eq!(r.broker.reconcile(), Ok(1));
    assert_eq!(fs_count(&r.cfg.state.join("live")), 1);
    r.script(&[record("done")], answered(None), &[HEAD]);
    let t = r.serve(2);
    let err = r.ask(&fix(HEAD)).unwrap_err();
    assert!(err.contains("won't stop"), "{err}");
    r.runner.0.lock().unwrap().stop_fails = false;
    r.ask(&fix(HEAD)).unwrap();
    t.join().unwrap();
    assert_eq!(r.runner.0.lock().unwrap().stopped, ["job-9"]);
    assert_eq!(fs_count(&r.cfg.state.join("live")), 0);
}
