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
//! first's); each turn's answers are eligible by what that turn held open
//! (proj's N.holds), apart from what the latest turn is doing; and each
//! still at work on a head someone else has moved the PR past, named to be
//! stopped, since whatever it does next is for a head the PR no longer has.
//! A job named to stop (that, or retired for a new fix job: `Look::retire`)
//! stays kept, retiring, and named again each pass until proj says it's
//! over; `Seen::clear` says when no kept job may still push. One still waiting for its slot can't have pushed. One at
//! work is named once GitHub's record of the branch says the head is
//! someone else's push, not the machinery App's its jobs push as
//! (answer.rs's `pusher`). Without that App its jobs push as the user, as
//! the PR's owner may, so a push can't be told apart: it carries on, and
//! what it leaves stands down then.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::answer::{self, Handed, Head, Posted, Took, Unposted, Wrote};
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
    /// A new fix job is about to be handed the PR: every job kept between
    /// turns is retired first (marked, and named to stop), and `clear` says
    /// when none is left.
    #[serde(default)]
    pub retire: bool,
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
    /// When (GitHub's clock, Unix seconds) its answers last made a write that proj's
    /// fingerprint sees (github.rs: a reply on a thread, a comment), or may
    /// have (one whose answer was lost): what wakes a job awaiting the PR.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub woke_at: Option<f64>,
    /// Its turn is over, awaiting this PR: proj resumes it on the PR's next
    /// change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waits: bool,
    /// It waits, and a write of its answers since that await began is the
    /// change that resumes it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumes: bool,
    /// To be retired: stopped, and kept (named to stop on every pass) until
    /// proj says it's over, so no job is handed the branch while it may
    /// still push. Set for a job someone else's push made stale, and for
    /// each waiting one when a new fix job is about to be handed the PR.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retiring: bool,
    /// The job may still push to the branch: at work, resumed by what went
    /// up, or retiring. The steward hands the branch to no other fix job
    /// while any does (step_jobs.py's look); it is the one word on it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub owns: bool,
}

impl Handing {
    /// Kept for its next turn, awaiting the PR, `resumes` when a write of its
    /// answers is the change that wakes it (`woken`).
    fn between(mut self, resumes: bool) -> Handing {
        self.waits = true;
        self.resumes = resumes;
        self
    }

    fn retire(&mut self) {
        self.retiring = true;
    }

    fn settle(mut self, at_work: bool) -> Handing {
        self.owns = at_work || self.resumes || self.retiring;
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
    /// No kept job may still push to the branch (none owns it).
    pub clear: bool,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Stop {
    pub jid: String,
    pub why: String,
}

/// What came of a job's answers: posted, nothing, wrong, unpushed, moved,
/// refused or unanswered (answer.rs), or pending (what its turn held can't
/// be read yet), and why. A refused, unanswered or pending one is kept, to
/// go again next pass.
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
    let own = format!("{}#{}", p.repo, l.number);
    for (jid, rec) in &l.answering {
        let mut rec = rec.clone();
        rec.waits = false;
        rec.resumes = false;
        rec.owns = false;
        // proj starts turns under this lock. Keep state, turn identity,
        // awaits and final answer collection in the same lifecycle snapshot.
        let opened = burrow_process::state_reads::Dir::open(&l.jobs)
            .and_then(|root| root.dir(std::path::Path::new(jid)))
            .and_then(|dir| dir.read_lock("lock").map(|lock| (dir, lock)));
        let Ok((dir, lock)) = opened else {
            keep(&mut seen, jid, rec, true);
            continue;
        };
        if lock.lock_shared().is_err() {
            keep(&mut seen, jid, rec, true);
            continue;
        }
        let life = p.jobs.lifecycle(jid);
        if rec.retiring {
            // Kept, and named again, until proj says the job is over: a
            // stop that failed (or a job resumed meanwhile) is never taken
            // for a retired owner of the branch.
            if life.at_work(None) {
                rec.retire();
                retire(&mut seen, jid, rec, "it is retiring from the branch");
            }
            continue;
        }
        let resumed = dir
            .text("turns")
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .is_some_and(|n| n > 1);
        let state = life.state.clone();
        // A completed turn that holds something open (awaiting upstream, or
        // a person's answer to its question) is still an active job. Keep
        // its original claim (including ownership checks) until its final
        // turn; intermediate answers may be replaced when proj resumes it.
        let at_work = life.at_work(Some(&own));
        // Its turn is over. Awaiting this PR, it's resumed on the PR's next
        // change: between turns, still at work on the branch, so a head
        // someone else pushed stops it as it would a running one.
        let between = !at_work && life.awaits(&own);
        let number = dir.text("turns").ok().and_then(|t| {
            let t = t.trim();
            (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()))
                .then(|| t.parse::<u64>().ok())
                .flatten()
                .filter(|n| *n > 0)
        });
        if (at_work || between)
            && rec.head != l.head
            && let Some(why) = stale(
                p,
                l,
                &mut seen,
                if at_work && !resumed {
                    state.as_deref()
                } else {
                    None
                },
                &mut rec,
            )
        {
            rec.retire();
            retire(&mut seen, jid, rec, &why);
            continue;
        }
        // Every turn's answers that may go up, each as its turn ended (proj
        // keeps them in N.answers beside N.holds, what the turn held open,
        // since its next turn rewrites answers.json before this pass can
        // look): those of a turn that held nothing, or only this PR. A
        // turn that held a question, an owner's call or something upstream
        // left its answers for the turn that replaces them. A latest turn
        // with none kept that is over is read from the file itself. In turn
        // order, each on the claim the one before left, whatever the latest
        // turn is doing now.
        // What a turn held is known only from its N.holds, which proj
        // writes before its N.answers (empty when it held nothing): one
        // missing or unreadable beside them says nothing, so that turn's
        // answers wait, and the turns after it with them, keeping turn
        // order, until it can be read.
        let latest = number.unwrap_or(0);
        let mut found: Vec<(String, String, String)> = Vec::new();
        let mut unreadable = None;
        let mut unknown = None;
        for n in 1..=latest {
            let name = format!("{n}.answers");
            let kept = dir.text(&name);
            let final_turn = n == latest && !at_work;
            let text = match kept {
                Ok(text) => {
                    if !final_turn {
                        match dir.text(&format!("{n}.holds")) {
                            Ok(holds) if holds_only(&holds, &own) => {}
                            Ok(_) => continue,
                            Err(e) => {
                                unknown = Some(format!(
                                    "what its turn {n} held can't be read ({e}): its answers wait for it"
                                ));
                                break;
                            }
                        }
                    }
                    Ok(text)
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && final_turn => {
                    dir.text("answers.json")
                }
                Err(e) => Err(e),
            };
            match text {
                Ok(text) => found.push((n.to_string(), digest(&text), text)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => unreadable = Some(e),
            }
        }
        if number.is_none() && !at_work {
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
        // Each turn's description, once its answers may have gone up (taken
        // on an earlier pass, or tried now), may be the PR's by this job's
        // own write, kept or not: the turns after it rest on it too.
        let mut also: Vec<String> = Vec::new();
        for (turn, sum, text) in found {
            let described = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v["description"].as_str().map(String::from));
            if rec.taken.contains(&sum) {
                also.extend(described);
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
                also: also.clone(),
                at: rec.at,
                answers: Some(text),
            };
            also.extend(described);
            let mut wrote = Wrote::default();
            let (outcome, why) = match answer::answer(p, &h, &mut wrote) {
                Ok(Posted::Answers(Took { head, description })) => {
                    rec.head = head;
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
            // Whatever came of the rest: the description it wrote is the
            // PR's now, so the turns after it are checked against it.
            if let Some(d) = wrote.description {
                rec.body = d;
            }
            if let Some(at) = wrote.at {
                rec.woke_at = Some(rec.woke_at.map_or(at as f64, |w| w.max(at as f64)));
            }
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
            // A turn whose head the PR has left (outcome "moved") says
            // nothing of the later turns, each checked on its own.
        }
        if let Some(why) = unknown
            && !retry
        {
            // Kept, to be read again next pass; never taken for held
            // nothing.
            seen.answered.push(Answered {
                jid: jid.clone(),
                kind: rec.kind.clone(),
                outcome: "pending",
                why: Some(why),
            });
            retry = true;
        }
        if at_work {
            keep(&mut seen, jid, rec, true);
        } else if between {
            let resumes = woken(p, l, &rec, await_line(&dir, &own));
            let mut rec = rec.between(resumes);
            if l.retire && !rec.resumes {
                rec.retire();
                retire(&mut seen, jid, rec, "a new fix job is to be handed the PR");
            } else {
                let owns = rec.resumes;
                keep(&mut seen, jid, rec, owns);
            }
        } else if retry {
            keep(&mut seen, jid, rec, false);
        }
    }
    seen.clear = !seen.answering.values().any(|r| r.owns);
    seen
}

/// A job kept, whether it may still push or not.
fn keep(seen: &mut Seen, jid: &str, rec: Handing, owns: bool) {
    seen.answering.insert(jid.to_owned(), rec.settle(owns));
}

/// A job to retire: kept, retiring, and named to stop.
fn retire(seen: &mut Seen, jid: &str, rec: Handing, why: &str) {
    seen.stop.push(Stop {
        jid: jid.to_owned(),
        why: why.to_owned(),
    });
    keep(seen, jid, rec, true);
}

/// Whether a turn's holds (proj's `N.holds`, a line each) are none, or only
/// the PR itself awaited.
fn holds_only(holds: &str, own: &str) -> bool {
    holds
        .lines()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .all(|h| {
            matches!(h.split_once(' '),
            Some(("await", target)) if target.trim().eq_ignore_ascii_case(own))
        })
}

/// The job's await of `own` (proj's `await` file: TARGET, since and its
/// fingerprint then, a tab apart): when it began, in Unix seconds, and what
/// the PR was then.
fn await_line(dir: &burrow_process::state_reads::Dir, own: &str) -> Option<(f64, String)> {
    dir.text("await").ok()?.lines().find_map(|l| {
        let mut f = l.splitn(3, '\t');
        let (target, since, was) = (f.next()?, f.next()?, f.next().unwrap_or_default());
        target.trim().eq_ignore_ascii_case(own).then_some(())?;
        Some((since.trim().parse::<f64>().ok()?, was.to_owned()))
    })
}

/// Whether a write of the job's answers is the change that wakes it from its
/// await: one made after the await began is; one made before it, in the
/// baseline it took, isn't. Both are whole seconds, so a write in the
/// await's own second is told apart by the baseline itself: the PR's
/// comment counts then, against what it counted.
fn woken(p: &mut Pass, l: &Look, rec: &Handing, line: Option<(f64, String)>) -> bool {
    let Some(w) = rec.woke_at else { return false };
    // Wrote, and the await can't be read: it may have woken it.
    let Some((since, was)) = line else {
        return true;
    };
    if w != since {
        return w > since;
    }
    let was: serde_json::Value = serde_json::from_str(&was).unwrap_or_default();
    let (Some(comments), Some(review)) =
        (was["comments"].as_u64(), was["review_comments"].as_u64())
    else {
        return true;
    };
    let now = |p: &mut Pass, path: String, key: &str| p.get(&path).ok()?[key].as_u64();
    let (repo, n) = (p.repo, l.number);
    let counts = (
        now(p, format!("repos/{repo}/issues/{n}"), "comments"),
        now(p, format!("repos/{repo}/pulls/{n}"), "review_comments"),
    );
    counts != (Some(comments), Some(review))
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
