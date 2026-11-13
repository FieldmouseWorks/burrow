//! Which shared CI runs judged this PR's current diff, and which later
//! runs replace cancelled work. Private to the CI verdict owner (ci.rs).

use std::collections::BTreeSet;

use burrow_github::encode;
use serde_json::Value;

use crate::ci::{Ask, array, text};
use crate::pass::Pass;

/// Prefix of the check naming the base a workflow judged.
pub const LANE: &str = "lane into ";
/// Prefix of a documentation-only gate, followed by its base and fork.
pub const DOCS: &str = "docs only into ";
pub(crate) const FLAKE: &str = ".github/workflows/flake.yml";

pub fn suite(c: &Value) -> &Value {
    &c["checkSuite"]
}

/// A check's logical workflow, with the shared gate's retarget workflow
/// folded into its caller so either can replace the other's cancelled run.
pub fn workflow(c: &Value) -> Option<&str> {
    suite_flow(suite(c))
}

pub(crate) fn suite_flow(s: &Value) -> Option<&str> {
    let run = &s["workflowRun"];
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

pub(crate) fn job(c: &Value) -> (&str, &str) {
    let name = text(&c["name"]).unwrap_or("");
    name.rsplit_once(" / ").unwrap_or(("", name))
}

/// Whether a job name denotes a whole gate, docs gate, or matrix leg.
pub fn gate(job: &str) -> bool {
    let leg = job.as_bytes();
    job == "check"
        || job.starts_with(DOCS)
        || (leg.len() == 10
            && &leg[..4] == b"leg "
            && (b'1'..=b'8').contains(&leg[4])
            && &leg[5..9] == b" of "
            && (b'2'..=b'8').contains(&leg[9]))
}

/// The same job, or explicit gate coverage in one validated shared call. A
/// docs verdict replaces only gate work, and the whole gate can cover a
/// previous docs job or leg. One leg never stands for a different leg.
fn same_work(a: &Value, b: &Value, shared: bool, docs: bool) -> bool {
    if a["name"] == b["name"] {
        return true;
    }
    let (ac, aj) = job(a);
    let (bc, bj) = job(b);
    if !shared || ac != bc {
        return false;
    }
    if aj.starts_with(LANE) && bj.starts_with(LANE) {
        return true;
    }
    ran(b) && ((gate(aj) && bj == "check") || (docs && gate(aj) && bj.starts_with(DOCS)))
}

fn sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn judged(c: &Value) -> Option<(&str, Option<&str>)> {
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

pub(crate) fn docs_lane(ask: &Ask) -> bool {
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

fn ran(c: &Value) -> bool {
    c["status"] == "COMPLETED" && c["conclusion"] != "SKIPPED" && c["conclusion"] != "CANCELLED"
}

fn id(c: &Value) -> Option<u64> {
    c["databaseId"].as_u64()
}

/// The checks that still speak for the head, after one ordered replacement
/// relation for failures and cancellations alike. A later check replaces
/// earlier work only in its own workflow and PR lane; skips did nothing;
/// a cancelled check waits for completed work. Only larger check ids
/// replace predecessors. Display names are not job ids: a name is usable
/// across suites only when unambiguous in both, and within one suite only
/// an explicit matching `externalId` identifies a retry.
pub fn effective<'a>(
    contexts: &'a [Value],
    stale: &BTreeSet<String>,
    docs: bool,
) -> Vec<&'a Value> {
    let checks: Vec<_> = contexts
        .iter()
        .filter(|c| c["__typename"] == "CheckRun")
        .collect();
    fn sid(c: &Value) -> Option<&str> {
        text(&suite(c)["id"])
    }
    let unique = |c: &Value| {
        let mut who = BTreeSet::new();
        for x in checks
            .iter()
            .filter(|x| sid(x) == sid(c) && x["name"] == c["name"])
        {
            who.insert(
                text(&x["externalId"])
                    .map(String::from)
                    .or_else(|| id(x).map(|i| i.to_string()))
                    .unwrap_or_else(|| format!("{:p}", *x)),
            );
        }
        who.len() == 1
    };
    let replaces = |c: &Value, x: &Value| {
        let (Some(mine), Some(newer), Some(flow)) = (id(c), id(x), workflow(c)) else {
            return false;
        };
        if newer <= mine
            || workflow(x) != Some(flow)
            || sid(c).is_none()
            || sid(x).is_none()
            || stale.contains(sid(x).unwrap_or(""))
            || x["conclusion"] == "SKIPPED"
            || !unique(c)
            || !unique(x)
        {
            return false;
        }
        if sid(c) == sid(x)
            && !(text(&c["externalId"]).is_some() && c["externalId"] == x["externalId"])
        {
            return false;
        }
        if cancelled(c) && (!ran(x) || suite(x)["status"] != "COMPLETED") {
            return false;
        }
        // A shared call is identified even when its lane was skipped before
        // GitHub filled in the name. A sibling call cannot vouch for it.
        let shared = checks.iter().any(|y| {
            workflow(y) == Some(flow) && job(y).0 == job(c).0 && job(y).1.starts_with(LANE)
        });
        if shared
            && !checks.iter().any(|y| {
                sid(y) == sid(x)
                    && job(y).0 == job(c).0
                    && ran(y)
                    && judged(y).is_some()
                    && !stale.contains(sid(y).unwrap_or(""))
            })
        {
            return false;
        }
        same_work(c, x, shared, docs)
    };
    contexts
        .iter()
        .filter(|c| {
            c["__typename"] != "CheckRun"
                || (!stale.contains(sid(c).unwrap_or("")) && !checks.iter().any(|x| replaces(c, x)))
        })
        .collect()
}
