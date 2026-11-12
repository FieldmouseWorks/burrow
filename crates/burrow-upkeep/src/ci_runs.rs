//! Which shared CI runs judged this PR's current diff, and which later
//! runs replace cancelled work. Private to the CI verdict owner (ci.rs).

use std::collections::BTreeSet;

use burrow_github::encode;
use serde_json::Value;

use crate::ci::{Ask, array, text};
use crate::pass::Pass;

const LANE: &str = "lane into ";
const DOCS: &str = "docs only into ";
const FLAKE: &str = ".github/workflows/flake.yml";

pub fn suite(c: &Value) -> &Value {
    &c["checkSuite"]
}

pub fn workflow(c: &Value) -> Option<&str> {
    let run = &suite(c)["workflowRun"];
    match text(&run["file"]["path"]) {
        Some(FLAKE | ".github/workflows/flake-retarget.yml") => Some(FLAKE),
        _ => text(&run["workflow"]["id"]),
    }
}

pub fn workflow_name(c: &Value) -> &str {
    text(&suite(c)["workflowRun"]["workflow"]["name"]).unwrap_or("CI")
}

pub fn cancelled(c: &Value) -> bool {
    c["status"] == "COMPLETED" && c["conclusion"] == "CANCELLED"
}

fn job(c: &Value) -> (&str, &str) {
    let name = text(&c["name"]).unwrap_or("");
    name.rsplit_once(" / ").unwrap_or(("", name))
}

fn gate(job: &str) -> bool {
    let leg = job.as_bytes();
    job == "check"
        || job.starts_with(DOCS)
        || (leg.len() == 10
            && &leg[..4] == b"leg "
            && (b'1'..=b'8').contains(&leg[4])
            && &leg[5..9] == b" of "
            && (b'2'..=b'8').contains(&leg[9]))
}

fn same_work(a: &Value, b: &Value) -> bool {
    if a["name"] == b["name"] {
        return true;
    }
    let (ac, aj) = job(a);
    let (bc, bj) = job(b);
    ac == bc && ((aj.starts_with(LANE) && bj.starts_with(LANE)) || (gate(aj) && gate(bj)))
}

fn sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn judged(c: &Value) -> Option<(&str, Option<&str>)> {
    if c["__typename"] != "CheckRun" || suite(c)["app"]["slug"] != "github-actions" {
        return None;
    }
    let (_, job) = job(c);
    if let Some(base) = job
        .strip_prefix(LANE)
        .filter(|s| !s.is_empty() && !s.contains(' '))
    {
        return Some((base, None));
    }
    let (base, fork) = job.strip_prefix(DOCS)?.split_once(" from ")?;
    (!base.is_empty() && !base.contains(' ') && sha(fork)).then_some((base, Some(fork)))
}

pub struct Runs {
    pub stale: BTreeSet<String>,
    pub docs: bool,
    pub unrun: BTreeSet<String>,
}

pub fn runs(p: &mut Pass, ask: &Ask, contexts: &[Value]) -> Runs {
    let base = text(&ask.pr["baseRefName"]).unwrap_or("");
    let said: Vec<_> = contexts
        .iter()
        .filter_map(|c| judged(c).map(|j| (c, j)))
        .collect();
    let mut fork = None;
    if ask.remote && said.iter().any(|(_, (b, f))| *b == base && f.is_some()) {
        let path = format!(
            "repos/{}/compare/{}...{}?per_page=1",
            p.repo,
            encode(base),
            encode(text(&ask.pr["headRefOid"]).unwrap_or(""))
        );
        // As before, a refused compare can't vouch for a docs lane. A
        // transient failure likewise leaves it untrusted, never green.
        if let Ok(got) = p.get(&path) {
            fork = text(&got["merge_base_commit"]["sha"])
                .filter(|s| sha(s))
                .map(String::from);
        }
    }
    let stale: BTreeSet<String> = said
        .iter()
        .filter(|(_, (b, f))| *b != base || f.is_some() && *f != fork.as_deref())
        .filter_map(|(c, _)| text(&suite(c)["id"]).map(String::from))
        .collect();
    let docs = said.iter().any(|(c, (_, f))| {
        f.is_some()
            && !stale.contains(text(&suite(c)["id"]).unwrap_or(""))
            && c["status"] == "COMPLETED"
            && c["conclusion"] == "SUCCESS"
    });
    let ran: BTreeSet<_> = said
        .iter()
        .filter(|(c, (_, f))| {
            f.is_none()
                && !stale.contains(text(&suite(c)["id"]).unwrap_or(""))
                && suite(c)["status"] == "COMPLETED"
        })
        .filter_map(|(c, _)| workflow(c))
        .collect();
    let unrun = said
        .iter()
        .filter(|(c, _)| {
            stale.contains(text(&suite(c)["id"]).unwrap_or(""))
                && !workflow(c).is_some_and(|w| ran.contains(w))
        })
        .map(|(c, _)| workflow_name(c).to_owned())
        .collect();
    Runs {
        stale,
        docs: docs && docs_lane(ask),
        unrun,
    }
}

fn docs_lane(ask: &Ask) -> bool {
    let Some(moved) = &ask.moved else {
        return false;
    };
    let files = array(&ask.pr["files"]["nodes"]);
    if ask.pr["changedFiles"].as_u64().unwrap_or(u64::MAX) > files.len() as u64 {
        return false;
    }
    !files
        .iter()
        .filter_map(|f| text(&f["path"]))
        .chain(moved.iter().map(String::as_str))
        .any(|p| p == ".github" || p.starts_with(".github/"))
}

#[derive(PartialEq)]
pub enum After {
    Finished,
    Going,
    None,
}

pub fn after(c: &Value, contexts: &[Value], stale: &BTreeSet<String>) -> After {
    let Some(mine) = text(&suite(c)["id"]) else {
        return After::None;
    };
    let Some(flow) = workflow(c) else {
        return After::None;
    };
    let same: Vec<_> = contexts
        .iter()
        .filter(|x| workflow(x) == Some(flow))
        .collect();
    let others: Vec<_> = same
        .iter()
        .copied()
        .filter(|x| text(&suite(x)["id"]) != Some(mine))
        .collect();
    let mut gone = stale.clone();
    gone.extend(
        others
            .iter()
            .filter(|x| cancelled(x))
            .filter_map(|x| text(&suite(x)["id"]).map(String::from)),
    );
    let done: Vec<_> = others
        .iter()
        .copied()
        .filter(|x| {
            suite(x)["status"] == "COMPLETED"
                && !gone.contains(text(&suite(x)["id"]).unwrap_or(""))
                && x["status"] == "COMPLETED"
                && x["conclusion"] != "SKIPPED"
                && x["conclusion"] != "CANCELLED"
        })
        .collect();
    let shared = same
        .iter()
        .any(|x| job(x).1.starts_with(LANE) && job(x).0 == job(c).0);
    if done.iter().any(|x| {
        same_work(c, x)
            && (!shared
                || done.iter().any(|lane| {
                    judged(lane).is_some()
                        && suite(lane)["id"] == suite(x)["id"]
                        && job(lane).0 == job(x).0
                }))
    }) {
        return After::Finished;
    }
    if others.iter().any(|x| suite(x)["status"] != "COMPLETED") {
        After::Going
    } else {
        After::None
    }
}
