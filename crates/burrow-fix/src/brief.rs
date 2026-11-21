//! What a fix job is told: the PR and the head it was handed, the problems
//! by their index, B1's policy for weighing findings (burrow-brief, never
//! a copy), and how it says what it did. The flow posts the answers,
//! resolves threads and asks for the next review: the job does none of it.
use conary_core::plugin::Problem;
use std::fmt::Write as _;

/// Where the job, by its own id, leaves its answers.
pub const ANSWERS: &str = "$PROJ_JOB/answers.json";

/// How a job takes a PR it was handed at `head` on `branch`, and pushes
/// to it: only on top of that head, never over anyone else's.
fn checkout(repo: &str, pr: u64, head: &str, branch: &str) -> String {
    format!(
        "You were handed it at {head}, on its branch {branch}. \
         First check it's still yours: if the PR's head (gh pr view {pr} -R {repo} --json headRefOid) \
         isn't {head}, someone else has pushed since, so push nothing, and say so in \"unable\" below. \
         Fetch it (git fetch origin {branch}) and work in a fresh worktree made at exactly that commit \
         (git worktree add --detach DIR {head}); push from there with git push origin HEAD:refs/heads/{branch}, \
         never with --force. If the push is refused because the branch moved, the new head is someone else's: \
         push nothing over it, and say so in \"unable\"."
    )
}

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
        "{repo}#{pr} needs a fix. {}\n\nThe problems, by their index:\n",
        checkout(repo, pr, head, branch)
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

/// The task for the stuck role on `repo`'s PR `pr`, handed at `head` on
/// `branch`, for the flow's reason and its own account of it: B1's policy
/// for weighing findings and the stuck role's ([`burrow_brief::STUCK`]),
/// never a copy.
pub fn stuck(
    repo: &str,
    pr: u64,
    head: &str,
    branch: &str,
    ask: &crate::stuck::Ask,
    jobs: &std::path::Path,
) -> String {
    let answers = format!("{}/{ANSWERS}", jobs.display());
    let marker = crate::stuck::marker(repo, pr);
    let mut out = format!(
        "{repo}#{pr} is stuck, and the flow hands it to you: {}.",
        ask.why.told()
    );
    if let Some(detail) = &ask.detail {
        let _ = write!(
            out,
            " What the flow saw, in its own words:\n\n{}\n",
            burrow_brief::quote(detail)
        );
    }
    let _ = write!(
        out,
        "\n{} {}\n\nRead the PR (gh pr view {pr} -R {repo} --comments, gh pr diff {pr} -R {repo}) and its review \
         threads (gh api graphql, the pullRequest's reviewThreads with their comments). For the findings: {} \
         Never delete, skip or weaken a test to turn a check green.\n\n\
         Findings you leave unfixed go to the PR's one follow-up issue. Find it first (gh issue list -R {repo} \
         --state open --search 'conary-stuck-followup in:body' --json number,body, and keep the one whose body has \
         the line {marker}) and edit it to add yours; only if there's none, open one (gh issue create -R {repo}) whose \
         body has that line, links the PR and lists each finding you left with your reason. \
         Don't reply on the PR's threads, resolve them, comment, edit its description, ask for a review or merge: \
         the flow does each of those, and links the follow-up issue on the findings you answered.\n\n\
         When you're done, write to {answers} as JSON, \
         {{\"pushed\": COMMIT, \"unable\": WHY, \"followup\": NUMBER, \"summary\": TEXT}}. pushed is the full commit \
         your push put on the branch (git rev-parse HEAD once git push has said it updated it), or null if you pushed \
         nothing. unable is why you couldn't settle the PR, or null; never both. followup is the follow-up issue's \
         number, or null if you left no finding unfixed. summary says what you did. End your turn done: this job is \
         one turn, so never await or ask.",
        burrow_brief::STUCK,
        checkout(repo, pr, head, branch),
        burrow_brief::FINDINGS.trim_end()
    );
    out
}
