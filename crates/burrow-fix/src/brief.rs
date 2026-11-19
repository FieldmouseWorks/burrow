//! What a fix job is told: the PR and the head it was handed, the problems
//! by their index, B1's policy for weighing findings (burrow-brief, never
//! a copy), and how it says what it did. The flow posts the answers,
//! resolves threads and asks for the next review: the job does none of it.
use conary_core::plugin::Problem;
use std::fmt::Write as _;

/// Where the job, by its own id, leaves its answers.
pub const ANSWERS: &str = "$PROJ_JOB/answers.json";

/// The task for a fix of `repo`'s PR `pr`, handed at `head` on `branch`.
pub fn task(
    repo: &str,
    pr: u64,
    head: &str,
    branch: &str,
    problems: &[Problem],
    jobs: &std::path::Path,
) -> String {
    let answers = format!("{}/{ANSWERS}", jobs.display());
    let mut out = format!(
        "{repo}#{pr} needs a fix. You were handed it at {head}, on its branch {branch}. \
         First check it's still yours: if the PR's head (gh pr view {pr} -R {repo} --json headRefOid) \
         isn't {head}, someone else has pushed since, so push nothing, and say so in \"unable\" below. \
         Fetch it (git fetch origin {branch}) and work in a fresh worktree made at exactly that commit \
         (git worktree add --detach DIR {head}); push from there with git push origin HEAD:refs/heads/{branch}, \
         never with --force. If the push is refused because the branch moved, the new head is someone else's: \
         push nothing over it, and say so in \"unable\".\n\nThe problems, by their index:\n"
    );
    for (i, p) in problems.iter().enumerate() {
        let _ = match p {
            Problem::Finding { finding } => writeln!(
                out,
                "{i}. A review finding ({:?}) at {}:{}: {}",
                finding.severity, finding.path, finding.line, finding.text
            ),
            Problem::RedCheck { check, text } => writeln!(
                out,
                "{i}. The check {check} is red: {text} Read its log, fix the cause and push."
            ),
            Problem::Conflict { base, text } => writeln!(
                out,
                "{i}. The branch conflicts with its base, {base}: {text} Merge the base in, resolve it and push."
            ),
        };
    }
    let _ = write!(
        out,
        "\nFor the findings: {} Never delete, skip or weaken a test to turn a check green.\n\n\
         Don't reply on the PR's threads, resolve them, comment, edit its description, ask for a review or merge: \
         the flow does each of those from your answers. When you're done, write them to {answers} as JSON, \
         {{\"pushed\": COMMIT, \"unable\": WHY, \"answers\": [{{\"problem\": INDEX, \"fixed\": BOOL, \"reply\": TEXT}}]}}. \
         pushed is the full commit your push put on the branch (git rev-parse HEAD once git push has said it \
         updated it), or null if you pushed nothing. unable is why you couldn't settle the problems, or null; \
         never both. answers has one entry for each review finding above and none for the rest, by its index, \
         in order: fixed is true only for one your push fixes, and reply says what you did, or why it's wrong \
         or out of scope, without naming a commit. End your turn done: this job is one turn, so never await \
         or ask.",
        burrow_brief::FINDINGS.trim_end()
    );
    out
}
