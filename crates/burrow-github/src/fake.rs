//! A GitHub that answers from a script and keeps what it was asked.

use super::*;
use std::collections::VecDeque;

type Answers = VecDeque<Result<Response, Failed>>;
/// What it was asked: method, path, body, etag.
pub type Asked = (String, String, Option<Value>, Option<String>);

#[derive(Default)]
pub struct Fake {
    /// A method and a path's start, and its answers in order; the last
    /// one stays.
    pub answers: Vec<(String, String, Answers)>,
    pub asked: Vec<Asked>,
    pub paused: Vec<u64>,
}

pub fn ok(status: u16, body: Value, headers: &[(&str, &str)]) -> Result<Response, Failed> {
    Ok(Response {
        status,
        headers: headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        body: if body.is_null() {
            String::new()
        } else {
            body.to_string()
        },
    })
}

impl Fake {
    pub fn on(
        &mut self,
        method: &str,
        path: &str,
        answers: Vec<Result<Response, Failed>>,
    ) -> &mut Self {
        self.answers
            .push((method.into(), path.into(), answers.into()));
        self
    }

    pub fn asked(&self, method: &str) -> Vec<&Asked> {
        self.asked.iter().filter(|a| a.0 == method).collect()
    }
}

impl Api for Fake {
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
            body.cloned(),
            etag.map(String::from),
        ));
        let (_, _, q) = self
            .answers
            .iter_mut()
            .find(|(m, p, _)| m == method && path.starts_with(p.as_str()))
            .unwrap_or_else(|| panic!("nothing scripted for {method} {path}"));
        if q.len() > 1 {
            q.pop_front().unwrap()
        } else {
            match q.front().unwrap() {
                Ok(r) => Ok(r.clone()),
                Err(f) => Err(Failed {
                    why: f.why.clone(),
                    transient: f.transient,
                }),
            }
        }
    }

    fn pause(&mut self, secs: u64) {
        self.paused.push(secs);
    }
}
