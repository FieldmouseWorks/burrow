//! The briefs' policy text, in one place (Conary's `docs/m6-plugins.md`,
//! B1): what a fix job must fix and may answer, and what a judge lands,
//! sends to the owner and counts as failing. The steward's tasks
//! (modules/autoland/tasks.py) read the fix job's and the judge's text
//! through the CLI ([`texts`]). No caller holds a copy. Codex reviews
//! through its GitHub connector, which reads the repo's AGENTS.md itself.

/// What a fix job must fix, and a judge counts as failing when a fix job
/// answered it away instead.
macro_rules! must_fix {
    () => {
        "a concrete defect (inputs or a state you can name that give a wrong result, a crash, lost data or a hole), or a break of a rule the Code Review Rules of the repo's AGENTS.md, as its base has them, list as P1"
    };
}

/// What must be fixed: a real defect or a rule the repo's reviewers flag
/// as P1 broken.
pub const MUST_FIX: &str = must_fix!();

/// How a fix job weighs review findings, the same from a PR's first round
/// to past its round limit: on their merits, not on their count or the
/// severity a reviewer gave them. What [`MUST_FIX`] is gets fixed in full;
/// the rest is answered with a reason, so no round goes on a finding that
/// names neither.
pub const FINDINGS: &str = concat!(
    "Weigh every finding on its merits, the same from the first round on, past the review-round limit too, ",
    "whatever severity the reviewer gave it. ",
    "Fix in full each one that is ",
    must_fix!(),
    ": all of what its finding says a complete fix covers, with a ",
    "regression test for each of those cases that fails without the fix, not only the line the thread is on. ",
    "Answer the rest with your reasons and change nothing for them: an edge no input or state can reach (say why ",
    "not), a style preference no rule asks for, or a hypothetical the finding gives no concrete failure scenario ",
    "for. A finding labelled P1 that names no rule on that P1 list, no reason in the PR's description that doesn't hold ",
    "(settled as below) and no concrete failure scenario counts as P2 here: its label alone doesn't make it one to fix. ",
);

/// What cleared a PR a judge is asked about, in the usual case.
pub const REVIEWED: &str = "its reviewers found nothing on that head, every thread is resolved";

/// What cleared a PR past its round limit: the reviewers weren't asked
/// again, so a fix job's answers stand in for their last look, and the
/// judge reads those too.
pub const PAST_LIMIT: &str = concat!(
    "it went past its review-round limit, so its reviewers weren't asked to look again and a Claude job ",
    "fixed or answered their last findings (read each thread as well as the diff: a finding answered away ",
    "that is ",
    must_fix!(),
    " counts as failing, the same as a failing build or test, so say which), ",
    "every thread is resolved",
);

/// What cleared a PR whose findings were answered without a push: its
/// reviewer won't look again unasked, and the steward can't ask (Codex's
/// connector takes a request only from a person with a Codex account), so
/// the answers stand in for its next look, and the judge reads those too.
pub const ANSWERED: &str = concat!(
    "its reviewers had findings on that head and every one was answered without a push, which they don't look at ",
    "again unless a person asks, so read each thread as well as the diff (a finding answered away that is ",
    must_fix!(),
    " counts as failing, the same as a failing build or test, so say which), ",
    "every thread is resolved",
);

/// How a reviewer and a judge weigh the reasons a PR's description gives
/// where the house rules ask for one. That the line is there is CI's to
/// check; whether its reason holds up takes reading the code.
pub const REASONS: &str = "Where the house rules ask a PR for a line in its description giving a reason (one starting `Why not Rust:` for new code that isn't Rust, say), judge the reason against the rule it answers and against the code, not only that it's there. Rust is the default, and a coin toss goes to Rust: a reason that only says it was quicker, that it's small, that the code around it is in that language already, or that the code itself contradicts (a \"throwaway\" script a service runs, \"glue\" with logic of its own) doesn't hold.";

/// A PR's description as a reviewer or a judge is shown it: each line
/// quoted, so nothing in it reads as their brief going on. What a review
/// carries of it is the SHA-256 of this (Conary's `flow::fingerprint`).
pub fn quote(body: &str) -> String {
    let quoted: Vec<String> = body
        .trim()
        .lines()
        .map(|l| format!("> {l}").trim_end().to_owned())
        .collect();
    if quoted.is_empty() {
        "(none)".into()
    } else {
        quoted.join("\n")
    }
}

/// What cleared a dependency update, which no reviewer reads: the judge
/// is the first to.
pub const DEP_SEEN: &str = "it's a dependency update, which no reviewer reads, so you're the first to read it; its threads are resolved";

/// What a judge counts as failing besides a failing build or test.
pub const FAILING: &str =
    "A reason in it that doesn't hold counts as failing, the same as a failing build or test.";

/// What a judge lands, and what it sends to `owner`.
pub fn land_or_owner(owner: &str) -> String {
    format!(
        "Land anything a revert or a follow-up PR can undo. Send it to {owner} only if it can't be undone (it drops or rewrites data for good, or exposes a secret) or it decides something the repo reserves for {owner}, and only if you genuinely can't make the call yourself."
    )
}

/// The fix job's and the judge's text, for `owner`, by name: what the
/// CLI prints for the steward's tasks.
pub fn texts(owner: &str) -> serde_json::Value {
    serde_json::json!({
        "must_fix": MUST_FIX,
        "findings": FINDINGS,
        "reviewed": REVIEWED,
        "past_limit": PAST_LIMIT,
        "answered": ANSWERED,
        "dep_seen": DEP_SEEN,
        "failing": FAILING,
        "land_or_owner": land_or_owner(owner),
        "reasons": REASONS,
    })
}

#[cfg(test)]
mod tests;
