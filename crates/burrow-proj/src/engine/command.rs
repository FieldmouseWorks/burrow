//! Generic turn prompt instructions shared by every engine plugin.

use crate::config::Config;
use crate::job::Job;
use crate::record::JobFile;
use crate::scratch;

/// How a job waits on its own long work, which its answer would stop.
pub const LONG: &str = "A command that can outlast one Bash call (ten minutes), a long build or test run, goes to the background with its output and exit code written to a file, and you wait for that file in the foreground, in steps under ten minutes each; answer only once it's there.";

/// How a turn says what comes next: what resumes the job, if anything.
pub const NEXT: &str = "`proj await URL` for each PR or issue you're still waiting on (the job is resumed, with this conversation, once one of them changes: checks finish, a review, comment or push, merged or closed), `proj ask --default \"TEXT\" [--one-way]` when your answer is a question only a person can settle (their reply resumes it; after the deadline a reversible default is taken, a one-way default is left for the owner), or `proj done` when the work is finished";

pub const FACTS: &str = "Check machine facts with `proj probe QUESTION...` before asking anyone; jobs never open probe issues.";
const SPLIT: &str = " An inbox task is one self-contained PR. If it is too big, publish progress and write 2 to 16 smaller self-contained tasks as a JSON array with title, body and check (and carries, OWNER/REPO#N, for a task that carries an open PR to landing: it is done once that PR merges), run `proj split FILE`, then end the session. Preserve the original authorization boundaries, outstanding work and PR links in those tasks; never carry a lane indefinitely.";

/// System-prompt instructions for a host `proj` turn.
pub(super) fn host_instructions(cfg: &Config, job: &Job, must: bool) -> String {
    let id = job.id;
    let scratch = scratch::dir(cfg, id);
    let keep = format!(
        "Any worktree, clone or build of your own goes in {} ($PROJ_SCRATCH), yours alone. Push what you mean to keep: it's removed, with everything in it, once this job is over,",
        scratch.display()
    );
    let instructions = if must {
        format!(
            "This is proj job {id}, running headless. Your turn ends with your answer, and anything you left running in the background is stopped then, so don't wait on anything in the background or with sleep. Before you answer, say what comes next, since nothing else ever resumes this job: run {NEXT}. Waiting on anything else (a background run, a notification) is waiting in this turn, in the foreground. {LONG} {keep} when a turn of it ends with `proj done`, or it's stopped."
        )
    } else {
        format!(
            "This is proj job {id}, running headless. Your turn ends with your answer, and anything you left running in the background is stopped then, so don't wait on GitHub (CI, reviews, merges) in the background or with sleep. Run `proj await URL` for each PR or issue you're still waiting on, then end your turn saying what you wait for: the job is resumed, with this conversation, once one of them changes (checks finish, a review, comment or push, merged or closed). {LONG} {keep} when a turn of it ends awaiting nothing, or it's stopped."
        )
    };
    let split = if job.file(JobFile::InboxIssue).exists()
        && cfg.inbox.as_ref().is_some_and(|i| i.planning)
    {
        SPLIT
    } else {
        ""
    };
    format!("{instructions} {FACTS}{split}")
}

/// System-prompt instructions for a sandboxed (`--vm`) `proj` turn.
pub(super) fn sandbox_instructions(must: bool) -> String {
    let next = if must {
        format!("Before answering, run {NEXT}.")
    } else {
        "If you are waiting on GitHub, run `proj await URL` for each target before answering."
            .into()
    };
    format!(
        "This is a sandboxed proj job, running headless. {next} The sandbox's proj records your declaration locally; the host registers GitHub waits when your run ends. Anything left running is stopped with the VM at the end of the turn. {LONG} {FACTS}"
    )
}
