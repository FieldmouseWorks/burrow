//! The runner, against a scripted job runner and the scripted GitHub,
//! behind the kit's socket as the producer reaches it.
use super::world::{World, read, world};
use super::{AUTHOR, HEAD, PR, REPO, ask_with, config, judge_request, negotiated};
use crate::runner::Runner;
use burrow_producer::jobs::{Jobs, Record as JobRecord, Turn};
use burrow_producer::serve::{Controller, Server, Work};
use conary_core::plugin::Trigger;
use serde_json::{Value, json};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// A job runner that runs nothing: each job's record is what the test
/// scripts, and what it was asked is kept.
#[derive(Clone, Default)]
struct Proj(Arc<Mutex<Jobbed>>);

#[derive(Default)]
struct Jobbed {
    started: Vec<(String, String, String)>,
    record: Option<JobRecord>,
    answers: Option<Value>,
    stopped: Vec<String>,
}

impl Jobs for Proj {
    fn start(&self, repo: &str, key: &str, task: &str) -> Result<String, String> {
        let mut j = self.0.lock().unwrap();
        j.started.push((repo.into(), key.into(), task.into()));
        Ok(format!("job-{}", j.started.len()))
    }
    fn find(&self, key: &str) -> Result<Option<String>, String> {
        let j = self.0.lock().unwrap();
        Ok(j.started
            .iter()
            .position(|s| s.1 == key)
            .map(|i| format!("job-{}", i + 1)))
    }
    fn why(&self, _: &str) -> Result<JobRecord, String> {
        self.0
            .lock()
            .unwrap()
            .record
            .clone()
            .ok_or("no record".into())
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

fn done(session: &str) -> JobRecord {
    JobRecord {
        stage: "done".into(),
        overdue_at: None,
        ends: vec!["done".into()],
        turn: Some(Turn {
            engine: Some("claude".into()),
            model: Some("a-model".into()),
            effort: Some("high".into()),
            session: Some(session.into()),
        }),
    }
}

fn runner(dir: &Path, w: &World, jobs: &Proj) -> Runner {
    let mut r = Runner::new(config(dir), Box::new(jobs.clone()), read(w)).unwrap();
    r.look = Duration::from_millis(10);
    r
}

#[test]
fn the_runner_answers_its_jobs_verdict_by_the_jobs_own_session() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let jobs = Proj::default();
    jobs.0.lock().unwrap().record = Some(done("job-session"));
    jobs.0.lock().unwrap().answers =
        Some(json!({"verdict": "misdescribed", "why": "It says more."}));
    let r = runner(dir.path(), &w, &jobs);
    let request = judge_request(&w, vec![Trigger::NoCi]);
    let got = ask_with(dir.path(), &|q, w: &Work| r.handle(q, w), &request, None).unwrap();
    assert_eq!(got["by"]["session"], "job-session");
    assert_eq!(got["execution"]["model"], "a-model");
    assert_eq!(got["verdict"], "misdescribed");
    assert_eq!(got["why"], "It says more.");
    let j = jobs.0.lock().unwrap();
    assert_eq!(j.started.len(), 1);
    assert!(j.started[0].1.starts_with("judge/acme/widget/5/"));
    assert!(
        j.started[0]
            .2
            .contains("run its build and its tests yourself")
    );
    assert!(
        j.started[0]
            .2
            .contains("> It ends on the empty line, by design.")
    );
    // Nothing is left for a restart to stop.
    assert_eq!(
        std::fs::read_dir(dir.path().join("runner/live"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn the_runner_refuses_a_discussion_that_isnt_the_requests_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let jobs = Proj::default();
    let r = runner(dir.path(), &w, &jobs);
    let request = judge_request(&w, vec![Trigger::Asked]);
    w.with(|p| p.threads.clear());
    assert!(ask_with(dir.path(), &|q, w: &Work| r.handle(q, w), &request, None).is_err());
    assert!(jobs.0.lock().unwrap().started.is_empty());
}

#[test]
fn a_judgment_whose_request_ends_stops_its_job_and_a_restart_stops_what_was_lost() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let jobs = Proj::default();
    let mut running = done("s");
    running.stage = "running".into();
    jobs.0.lock().unwrap().record = Some(running);
    let r = Arc::new(runner(dir.path(), &w, &jobs));
    // Served behind its socket, as the producer reaches it; the producer's
    // connection closing is its request ending.
    let me = Controller(rustix::process::getuid().as_raw());
    let server = Server::bind(&dir.path().join("runner.sock"), me).unwrap();
    let served = r.clone();
    let serving = thread::spawn(move || {
        server
            .once(&move |request, work: &Work| served.handle(request, work))
            .unwrap();
    });
    let mut stream = UnixStream::connect(dir.path().join("runner.sock")).unwrap();
    let mut line = serde_json::to_vec(&json!({
        "contract": conary_core::plugin::CONTRACT,
        "execution": {"choice": {"engine": "claude", "model": "a-model", "effort": "high"},
            "context": {"kind": "fresh"}},
        "negotiated": negotiated(),
        "kind": "judge", "repo": REPO, "pr": PR, "head": HEAD, "against": w.against(),
        "author": {"engine": "claude", "session": AUTHOR}, "reviewers": [],
        "why": ["asked"], "context": w.context(),
    }))
    .unwrap();
    line.push(b'\n');
    std::io::Write::write_all(&mut stream, &line).unwrap();
    for _ in 0..500 {
        if !jobs.0.lock().unwrap().started.is_empty() || serving.is_finished() {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    if serving.is_finished() {
        let mut reply = String::new();
        std::io::BufRead::read_line(&mut std::io::BufReader::new(&stream), &mut reply).unwrap();
        panic!("it answered before its job started: {reply}");
    }
    drop(stream);
    serving.join().unwrap();
    assert_eq!(jobs.0.lock().unwrap().stopped, ["job-1"]);

    // A job a crash left waiting is stopped at the next start.
    std::fs::write(dir.path().join("runner/live/lost"), "judge/acme/widget/5/1").unwrap();
    jobs.0
        .lock()
        .unwrap()
        .started
        .push((REPO.into(), "judge/acme/widget/5/1".into(), "t".into()));
    let r = runner(dir.path(), &w, &jobs);
    assert_eq!(r.reconcile().unwrap(), 1);
    assert_eq!(jobs.0.lock().unwrap().stopped, ["job-1", "job-2"]);
    assert_eq!(
        std::fs::read_dir(dir.path().join("runner/live"))
            .unwrap()
            .count(),
        0
    );
}
