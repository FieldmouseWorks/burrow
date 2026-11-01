//! The steward's Claude jobs that settle a PR, as the PR's pass sees to
//! them (claims.py hands over what it keeps of each, and keeps what this
//! gives back): what each that's over left, posted (answer.rs) or why not;
//! and each still at work on a head someone else has moved the PR past,
//! named to be stopped, since whatever it does next is for a head the PR no
//! longer has. One still waiting for its slot can't have pushed. One at
//! work is named once GitHub's record of the branch says the head is
//! someone else's push, not the machinery App's its jobs push as
//! (answer.rs's `pusher`). Without that App its jobs push as the user, as
//! the PR's owner may, so a push can't be told apart: it carries on, and
//! what it leaves stands down then.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::answer::{self, Handed, Head, Posted, Unposted};
use crate::pass::Pass;

/// Each looked for the push behind a head this many times at most, a few
/// minutes apart, while GitHub's record of the branch doesn't have it yet.
pub const LOOKS: u32 = 3;
/// How far apart, in seconds.
const LOOK_AGAIN: f64 = 120.0;

/// What the pass hands `claims`, on stdin.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Look {
    pub number: u64,
    /// The PR's head now, and its branch.
    pub head: String,
    pub branch: String,
    /// The login its jobs push as (`SLUG[bot]`), or None without that App.
    pub machinery: Option<String>,
    /// proj's jobs dir: each job leaves its answers in its own folder there.
    pub jobs: PathBuf,
    /// Its jobs, by id, as the PR's state keeps them.
    pub answering: BTreeMap<String, Handing>,
    /// Who pushed a head, as last found.
    pub pushed_by: Option<(String, String)>,
}

/// A job, as it was handed the PR.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Handing {
    /// The steward's kind of fix job (handed, redFixed ...).
    pub kind: String,
    /// The PR's head, its description, and when, in Unix seconds.
    pub head: String,
    pub body: String,
    pub at: f64,
    /// How many times its pass has looked for who pushed each head.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub looked: BTreeMap<String, u32>,
}

/// What `claims` gives back: the jobs to keep, who pushed the head, the
/// jobs to stop, and what came of those that were over.
#[derive(Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Seen {
    pub answering: BTreeMap<String, Handing>,
    pub pushed_by: Option<(String, String)>,
    pub stop: Vec<Stop>,
    pub answered: Vec<Answered>,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Stop {
    pub jid: String,
    pub why: String,
}

/// What came of a job's answers: posted, nothing, wrong, unpushed, moved,
/// refused or unanswered (answer.rs), and why. A refused or unanswered one
/// is kept, to go again next pass.
#[derive(Debug, PartialEq, Serialize)]
pub struct Answered {
    pub jid: String,
    pub kind: String,
    pub outcome: &'static str,
    pub why: Option<String>,
}

pub fn claims(p: &mut Pass, l: &Look) -> Seen {
    let mut seen = Seen {
        pushed_by: l.pushed_by.clone(),
        ..Seen::default()
    };
    for (jid, rec) in &l.answering {
        let mut rec = rec.clone();
        let state = p.jobs.state(jid);
        if matches!(state.as_deref(), None | Some("queued" | "running")) {
            if rec.head != l.head
                && let Some(why) = stale(p, l, &mut seen, state.as_deref(), &mut rec)
            {
                seen.stop.push(Stop {
                    jid: jid.clone(),
                    why,
                });
            } else {
                seen.answering.insert(jid.clone(), rec);
            }
            continue;
        }
        let path = l.jobs.join(jid).join("answers.json");
        let answers = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                seen.answered.push(Answered {
                    jid: jid.clone(),
                    kind: rec.kind.clone(),
                    outcome: "wrong",
                    why: Some(format!("its answers file can't be read: {e}")),
                });
                continue;
            }
        };
        let h = Handed {
            number: l.number,
            head: rec.head.clone(),
            description: rec.body.clone(),
            at: rec.at,
            answers,
        };
        let (outcome, why) = match answer::answer(p, &h) {
            Ok(Posted::Answers) => ("posted", None),
            Ok(Posted::Nothing) => ("nothing", None),
            Err(Unposted::Wrong(why)) => ("wrong", Some(why)),
            Err(Unposted::Unpushed(why)) => ("unpushed", Some(why)),
            Err(Unposted::Moved(why)) => ("moved", Some(why)),
            Err(Unposted::Refused(why)) => ("refused", Some(why)),
            Err(Unposted::Unanswered(why)) => ("unanswered", Some(why)),
        };
        if matches!(outcome, "refused" | "unanswered") {
            seen.answering.insert(jid.clone(), rec.clone());
        }
        if outcome != "nothing" {
            seen.answered.push(Answered {
                jid: jid.clone(),
                kind: rec.kind,
                outcome,
                why,
            });
        }
    }
    seen
}

fn short(c: &str) -> &str {
    c.get(..7).unwrap_or(c)
}

/// Why a job at work on a head the PR no longer has is to stop, or None:
/// it's still waiting for its slot, or the PR's head is someone other than
/// the machinery App's push.
fn stale(
    p: &mut Pass,
    l: &Look,
    seen: &mut Seen,
    state: Option<&str>,
    rec: &mut Handing,
) -> Option<String> {
    let now = short(&l.head);
    if state == Some("queued") {
        return Some(format!(
            "it was queued for a head the PR no longer has (it's at {now} now)"
        ));
    }
    let bot = l.machinery.as_deref()?;
    let by = match &seen.pushed_by {
        Some((head, by)) if *head == l.head => by.clone(),
        _ => {
            let h = Head {
                branch: l.branch.clone(),
                head: l.head.clone(),
            };
            let said = match answer::pusher(p, &h) {
                Ok(Some(by)) => {
                    seen.pushed_by = Some((l.head.clone(), by.clone()));
                    Ok(by)
                }
                Ok(None) => Err("GitHub has no push that left it there".to_string()),
                Err(e) => Err(p.noted(e).why),
            };
            match said {
                Ok(by) => by,
                Err(why) => {
                    // GitHub's record lags a push a little.
                    let looked = rec.looked.entry(l.head.clone()).or_insert(0);
                    *looked += 1;
                    if *looked < LOOKS {
                        // ends: the next look for who pushed it, LOOKS at most
                        p.wake(p.now + LOOK_AGAIN);
                    }
                    eprintln!(
                        "{}#{}: can't tell who pushed {now}: {why}",
                        p.repo, l.number
                    );
                    return None;
                }
            }
        }
    };
    (by != bot).then(|| format!("{now}, the PR's head now, is {by}'s push, not its own"))
}

#[cfg(test)]
#[path = "claims_tests.rs"]
mod tests;
