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

/// Why the flow hands a PR to the stuck role (Conary's FP11, burrow#945):
/// a review loop that won't settle, or a stall the flow couldn't bring
/// back. The word is how the flow names it in its request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stuck {
    /// Its reviewers still had findings after the configured rounds.
    RoundLimit,
    /// Its fixes grew the diff past the configured share of what it was.
    FixGrowth,
    /// A finding a fix had answered was raised again, unchanged.
    RaisedAgain,
    /// A stage stalled, and the flow couldn't bring it back.
    Stall,
}

impl Stuck {
    pub const ALL: [Self; 4] = [
        Self::RoundLimit,
        Self::FixGrowth,
        Self::RaisedAgain,
        Self::Stall,
    ];

    /// Its word in the flow's request.
    pub fn word(self) -> &'static str {
        match self {
            Self::RoundLimit => "round_limit",
            Self::FixGrowth => "fix_growth",
            Self::RaisedAgain => "raised_again",
            Self::Stall => "stall",
        }
    }

    /// The reason a word names, if it names one.
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.word() == word)
    }

    /// What it means, as the stuck role is told it.
    pub fn told(self) -> &'static str {
        match self {
            Self::RoundLimit => {
                "its reviewers still had findings after the review-round limit, so no reviewer is asked again and no other fix round runs"
            }
            Self::FixGrowth => {
                "its fix rounds grew the diff past the share of the original change they may add, so no other fix round runs"
            }
            Self::RaisedAgain => {
                "a reviewer raised again, unchanged, a finding a fix had already answered, so no other fix round runs"
            }
            Self::Stall => {
                "one of its stages stalled, and the flow couldn't bring it back the ways GitHub allows: if the PR's own change is why, fix that, and if it isn't, push nothing and say what it is"
            }
        }
    }
}

/// What the stuck role does with a PR the flow handed it: settle it in one
/// push, or say why it can't, and put what it won't fix in one follow-up
/// issue, never in another round.
pub const STUCK: &str = concat!(
    "You break the loop: you settle the PR in one push, or say why you can't. ",
    "Read every review finding still open on it, and every answer a fix gave, as well as the diff. ",
    "Fix the blockers among them in one push and leave the rest for one follow-up issue, which a later PR takes up. ",
    "You don't judge it: its head gets one more review, the findings on that head go to a judge, ",
    "and CI and every landing gate still hold.",
);

/// The fix job's and the judge's text, for `owner`, by name: what the
/// CLI prints for the steward's tasks.
pub fn texts(owner: &str) -> serde_json::Value {
    serde_json::json!({
        "must_fix": MUST_FIX,
        "findings": FINDINGS,
        "reviewed": REVIEWED,
        "past_limit": PAST_LIMIT,
        "dep_seen": DEP_SEEN,
        "failing": FAILING,
        "land_or_owner": land_or_owner(owner),
        "reasons": REASONS,
    })
}

#[cfg(test)]
mod tests;
