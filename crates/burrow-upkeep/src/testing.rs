//! What the parts' tests share: a config in a temp dir, a GitHub that
//! answers from what a test says it has (and keeps what it was asked), and
//! Claude jobs whose states a test sets.

use std::collections::HashMap;
use std::path::Path;

use burrow_github::{Api, Failed, Response};
use serde_json::{Value, json};

use crate::config::{Config, Dashboard, Flaky};
use crate::pass::{Jobs, Out, Pass};

pub const T: f64 = 1_800_000_000.0;

pub fn config(dir: &Path) -> Config {
    Config {
        state_dir: dir.join("state"),
        proj: "proj".into(),
        projects_dir: dir.join("projects").display().to_string(),
        authors: vec!["sam".into()],
        bots: vec!["dependabot".into(), "renovate".into(), "workers".into()],
        app: "steward".into(),
        job_timeout: 60,
        renovate: None,
        dashboard: Some(Dashboard {
            title: "Dependency Dashboard".into(),
            wait_days: 4,
            error_days: 1,
            renovate_kick: Some("http://127.0.0.1:9/kick/renovate".into()),
            renovate_runs: Some(dir.join("renovate-runs")),
        }),
        flaky: Some(Flaky { job_timeout: 240 }),
    }
}

/// Each answer GitHub gives, by a method and a path's start: the first that
/// matches answers. An answer with a status of 0 is none at all (gh's
/// failure, `why`).
#[derive(Default)]
pub struct Hub {
    pub answers: Vec<(String, String, Answer)>,
    /// What it was asked: method, path, etag, body.
    pub asked: Vec<(String, String, Option<String>, Option<Value>)>,
    /// GraphQL's answer to an issue's query, by its number.
    pub issues: HashMap<u64, Value>,
}

#[derive(Clone)]
pub struct Answer {
    pub status: u16,
    pub body: Value,
    pub etag: Option<String>,
    pub why: String,
}

pub fn answer(status: u16, body: Value) -> Answer {
    Answer {
        status,
        body,
        etag: None,
        why: String::new(),
    }
}

impl Hub {
    /// From now on, `method path...` answers this (ahead of what it said
    /// before).
    pub fn on(&mut self, method: &str, path: &str, a: Answer) {
        self.answers.retain(|(m, p, _)| !(m == method && p == path));
        self.answers.insert(0, (method.into(), path.into(), a));
    }

    pub fn paths(&self) -> Vec<&str> {
        self.asked.iter().map(|a| a.1.as_str()).collect()
    }
}

impl Api for Hub {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> Result<Response, Failed> {
        self.asked.push((
            method.into(),
            path.into(),
            etag.map(String::from),
            body.cloned(),
        ));
        if (method, path) == ("POST", "graphql")
            && let Some(n) = body.and_then(|b| b["variables"]["n"].as_u64())
            && let Some(issue) = self.issues.get(&n)
        {
            let data = json!({"data": {"repository": {"issue": issue}}});
            return Ok(response(200, &data, None));
        }
        let Some((_, _, a)) = self
            .answers
            .iter()
            .find(|(m, p, _)| m == method && path.starts_with(p.as_str()))
        else {
            panic!("nothing said for {method} {path}");
        };
        if a.status == 0 {
            return Err(Failed {
                why: a.why.clone(),
                transient: false,
            });
        }
        // GitHub's conditional request: unchanged since that etag, a 304.
        if a.etag.is_some() && etag == a.etag.as_deref() {
            return Ok(response(304, &Value::Null, a.etag.as_deref()));
        }
        Ok(response(a.status, &a.body, a.etag.as_deref()))
    }

    fn pause(&mut self, _: u64) {}
}

fn response(status: u16, body: &Value, etag: Option<&str>) -> Response {
    Response {
        status,
        headers: etag
            .map(|e| ("ETag".into(), e.into()))
            .into_iter()
            .collect(),
        body: if body.is_null() {
            String::new()
        } else {
            body.to_string()
        },
    }
}

/// Claude jobs' states, as a test sets them, else `default`; and the jobs
/// proj has under a key (`found`), or why it can't say (`unfound`).
pub struct States {
    pub of: HashMap<String, String>,
    pub default: Option<String>,
    pub asked: Vec<String>,
    pub found: HashMap<String, Vec<String>>,
    pub unfound: Option<String>,
    pub finds: Vec<String>,
}

impl States {
    pub fn new(default: &str) -> States {
        States {
            of: HashMap::new(),
            default: Some(default.into()),
            asked: vec![],
            found: HashMap::new(),
            unfound: None,
            finds: vec![],
        }
    }

    /// The job the part's last look asked for started, as `jid`: proj has
    /// it under the key that look gave it.
    pub fn start(&mut self, cfg: &Config, part: &str, jid: &str) {
        let text = std::fs::read_to_string(cfg.state_dir.join(format!("{part}-sam_r.json")));
        let st: Value = serde_json::from_str(&text.unwrap()).unwrap();
        let at = st["askedAt"].as_f64().expect("a job asked for");
        let key = crate::pass::key("sam/r", part, at);
        self.found.entry(key).or_default().push(jid.into());
    }

    pub fn set(&mut self, jid: &str, state: &str) {
        self.of.insert(jid.into(), state.into());
    }
}

impl Jobs for States {
    fn state(&mut self, jid: &str) -> Option<String> {
        self.asked.push(jid.into());
        self.of.get(jid).cloned().or_else(|| self.default.clone())
    }

    fn find(&mut self, key: &str) -> Result<Vec<String>, String> {
        self.finds.push(key.into());
        match &self.unfound {
            Some(why) => Err(why.clone()),
            None => Ok(self.found.get(key).cloned().unwrap_or_default()),
        }
    }
}

/// One look, `at` seconds after T, with `f`: what it says back.
pub fn look<R>(
    cfg: &Config,
    hub: &mut Hub,
    jobs: &mut States,
    at: f64,
    f: impl FnOnce(&mut Pass) -> Result<R, String>,
) -> (Out, Result<R, String>) {
    std::fs::create_dir_all(&cfg.state_dir).unwrap();
    hub.asked.clear();
    let mut p = Pass {
        cfg,
        repo: "sam/r",
        now: T + at,
        dry: false,
        searched: 0,
        api: hub,
        jobs,
        out: Out::default(),
    };
    let r = f(&mut p);
    (p.out, r)
}

/// The job the part's look asked for started, as `jid`, and a look `at`
/// seconds after T has found it.
pub fn start(cfg: &Config, at: f64, part: &str, jid: &str) {
    let mut hub = Hub::default();
    let mut jobs = States::new("running");
    jobs.start(cfg, part, jid);
    let (_, r) = look(cfg, &mut hub, &mut jobs, at, |p| match part {
        "dashboard" => crate::dashboard::found(p),
        _ => crate::flaky::found(p),
    });
    r.unwrap();
}
