//! The steward's Claude jobs that settle a PR, as the PR's pass sees to
//! them (claims.py hands over what it keeps of each, and keeps what this
//! gives back): what each that's over left, posted (answer.rs) or why not;
//! a done turn holding something open (awaiting upstream, or a person's
//! answer to its question) stays claimed until its final turn, with
//! any intermediate answers left unposted for that turn to replace;
//! a done turn awaiting the PR itself has its answers posted, since that
//! PR's next change (what went up, CI, a review) is what resumes it, and
//! stays claimed, so each later turn's new answers go up in their turn and
//! the ones already taken never again (PSIV#459's second turn pushed the
//! fix, and its answers were never read: its claim had gone with its
//! first's); and each still at work on a head someone else has moved the PR past,
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
use sha2::{Digest, Sha256};

use crate::answer::{self, Handed, Head, Posted, Took, Unposted};
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
    /// The PR's head, its description, and when, in Unix seconds; once
    /// answers of its went up, the head and description they left.
    pub head: String,
    pub body: String,
    pub at: f64,
    /// How many times its pass has looked for who pushed each head.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub looked: BTreeMap<String, u32>,
    /// Every answers file whose answers were dealt with (posted, or found
    /// wrong), by its SHA-256: never again, whatever came between.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub taken: Vec<String>,
    /// The turn whose answers last went up with a comment, the one change
    /// of theirs that proj's fingerprint sees (github.rs), so the one that
    /// wakes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub posted: Option<u64>,
    /// Its turn is over, awaiting this PR: proj resumes it on the PR's next
    /// change. A new fix job handed the PR stops it first (claims.py).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waits: bool,
    /// It waits, and that turn's answers went up: what went up is the
    /// change that resumes it, so it's still at work, and no other fix job
    /// is handed the PR meanwhile (step_jobs.py's look). One whose turn left
    /// nothing new has ended for the steward, as before.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumes: bool,
}

impl Handing {
    /// Kept for its next turn, awaiting the PR as of `turn` (if it can be
    /// read).
    fn between(mut self, turn: Option<u64>) -> Handing {
        self.waits = true;
        self.resumes = turn.is_some() && self.posted == turn;
        self
    }
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
        rec.waits = false;
        rec.resumes = false;
        // proj starts turns under this lock. Keep state, turn identity,
        // awaits and final answer collection in the same lifecycle snapshot.
        let opened = burrow_process::state_reads::Dir::open(&l.jobs)
            .and_then(|root| root.dir(std::path::Path::new(jid)))
            .and_then(|dir| dir.read_lock("lock").map(|lock| (dir, lock)));
        let Ok((dir, lock)) = opened else {
            seen.answering.insert(jid.clone(), rec);
            continue;
        };
        if lock.lock_shared().is_err() {
            seen.answering.insert(jid.clone(), rec);
            continue;
        }
        let resumed = dir
            .text("turns")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .is_some_and(|n| n > 1);
        let life = p.jobs.lifecycle(jid);
        let state = life.state.clone();
        // A completed turn that holds something open (awaiting upstream, or
        // a person's answer to its question) is still an active job. Keep
        // its original claim (including ownership checks) until its final
        // turn; intermediate answers may be replaced when proj resumes it.
        let own = format!("{}#{}", p.repo, l.number);
        if life.at_work(Some(&own)) {
            if rec.head != l.head
                && let Some(why) = stale(
                    p,
                    l,
                    &mut seen,
                    if resumed { None } else { state.as_deref() },
                    &mut rec,
                )
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
        // Its turn is over. Awaiting this PR, it's resumed on the PR's next
        // change: between turns, still at work on the branch, so a head
        // someone else pushed stops it as it would a running one.
        let between = life.awaits(&own);
        let number = dir.text("turns").ok().and_then(|t| {
            let t = t.trim();
            (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
                .then(|| t.parse::<u64>().ok())
                .flatten()
                .filter(|n| *n > 0)
        });
        if between
            && rec.head != l.head
            && let Some(why) = stale(p, l, &mut seen, None, &mut rec)
        {
            seen.stop.push(Stop {
                jid: jid.clone(),
                why,
            });
            continue;
        }
        // Every turn's answers, each as its turn ended (proj keeps them in
        // N.answers, since its next turn rewrites answers.json before this
        // pass can look); a latest turn with none kept is read from the
        // file itself. In turn order, each on the claim the one before left.
        let latest = number.unwrap_or(0);
        let mut found: Vec<(String, String, String)> = Vec::new();
        let mut unreadable = None;
        for n in 1..=latest {
            let name = format!("{n}.answers");
            let name = if dir.text(&name).is_ok() || n < latest {
                name
            } else {
                "answers.json".to_owned()
            };
            match dir.text(&name) {
                Ok(text) => found.push((n.to_string(), digest(&text), text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => unreadable = Some(e),
            }
        }
        if number.is_none() {
            match dir.text("answers.json") {
                Ok(text) => found.push(("invalid-turn".into(), digest(&text), text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => unreadable = Some(e),
            }
        }
        if let Some(e) = unreadable {
            seen.answered.push(Answered {
                jid: jid.clone(),
                kind: rec.kind.clone(),
                outcome: "wrong",
                why: Some(format!("its answers file can't be read: {e}")),
            });
        }
        let mut retry = false;
        for (turn, sum, text) in found {
            if rec.taken.contains(&sum) {
                continue;
            }
            let engine = dir.text(&format!("{turn}.engine")).unwrap_or_default();
            let engine = match engine.trim() {
                "codex" => "Codex",
                "claude" | "" => "Claude",
                _ => "Unknown",
            }
            .to_owned();
            let h = Handed {
                engine,
                number: l.number,
                head: rec.head.clone(),
                description: rec.body.clone(),
                at: rec.at,
                answers: Some(text),
            };
            let (outcome, why) = match answer::answer(p, &h) {
                Ok(Posted::Answers(Took {
                    head,
                    description,
                    woke,
                })) => {
                    rec.head = head;
                    if woke {
                        rec.posted = turn.parse().ok();
                    }
                    if let Some(d) = description {
                        rec.body = d;
                    }
                    ("posted", None)
                }
                Ok(Posted::Nothing) => ("nothing", None),
                Err(Unposted::Wrong(why)) => ("wrong", Some(why)),
                Err(Unposted::Unpushed(why)) => ("unpushed", Some(why)),
                Err(Unposted::Moved(why)) => ("moved", Some(why)),
                Err(Unposted::Refused(why)) => ("refused", Some(why)),
                Err(Unposted::Unanswered(why)) => ("unanswered", Some(why)),
            };
            let again = matches!(outcome, "refused" | "unanswered");
            if !again && outcome != "nothing" {
                rec.taken.push(sum);
            }
            if outcome != "nothing" {
                seen.answered.push(Answered {
                    jid: jid.clone(),
                    kind: rec.kind.clone(),
                    outcome,
                    why,
                });
            }
            if again {
                // Retried next pass, with the turns after it.
                retry = true;
                break;
            }
            if outcome == "moved" {
                break;
            }
        }
        if between {
            seen.answering.insert(jid.clone(), rec.between(number));
        } else if retry {
            seen.answering.insert(jid.clone(), rec);
        }
    }
    seen
}

fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
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
