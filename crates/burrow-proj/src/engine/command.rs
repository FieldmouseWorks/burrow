//! Engine argv and environment, inside the turn's timeout and login shell.

use super::machine;
use super::{Choice, Engine, api, stream::resume};
use crate::record::{JobFile, TurnFile};
use crate::{config::Config, job::Job, scratch};

use std::path::Path;
use std::process::{Child, Command};

/// How a job waits on its own long work, which its answer would stop.
pub const LONG: &str = "A command that can outlast one Bash call (ten minutes), a long build or test run, goes to the background with its output and exit code written to a file, and you wait for that file in the foreground, in steps under ten minutes each; answer only once it's there.";

/// How a turn says what comes next: what resumes the job, if anything.
pub const NEXT: &str = "`proj await URL` for each PR or issue you're still waiting on (the job is resumed, with this conversation, once one of them changes: checks finish, a review, comment or push, merged or closed), `proj ask --default \"TEXT\" [--one-way]` when your answer is a question only a person can settle (their reply resumes it; after the deadline a reversible default is taken, a one-way default is left for the owner), or `proj done` when the work is finished";

pub const FACTS: &str = "Check machine facts with `proj probe QUESTION...` before asking anyone; jobs never open probe issues.";
const SPLIT: &str = " An inbox task is one self-contained PR. If it is too big, publish progress and write 2 to 16 smaller self-contained tasks as a JSON array with title, body and check (and carries, OWNER/REPO#N, for a task that carries an open PR to landing: it is done once that PR merges), run `proj split FILE`, then end the session. Preserve the original authorization boundaries, outstanding work and PR links in those tasks; never carry a lane indefinitely.";

/// Claude's flags for the turn: its stream, the effort, the session it
/// resumes and the model.
fn args(choice: &Choice, session: Option<String>) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--disable-slash-commands",
    ]
    .map(String::from)
    .to_vec();
    if let Some(e) = &choice.effort {
        args.extend(["--effort".into(), e.clone()]);
    }
    if let Some(s) = session {
        args.extend(["--resume".into(), s]);
    }
    if let Some(m) = &choice.model {
        args.extend(["--model".into(), m.clone()]);
    }
    args
}

/// One run of Claude, its output appended to the turn's stream. It goes
/// under timeout(1) with no limit of its own: stopping it through that
/// stops all of it (TERM, then KILL a minute on), as timeout(1) does at a
/// limit.
#[allow(clippy::too_many_arguments)]
pub fn spawn(
    cfg: &Config,
    job: &Job,
    n: &str,
    prompt: &Path,
    waitdir: &Path,
    agent: Option<&str>,
    must: bool,
    app_env: &str,
    choice: &Choice,
) -> std::io::Result<Child> {
    let mut args = args(choice, resume(job, choice.engine));
    if choice.engine == Engine::ClaudeApi && agent.is_some() {
        return Err(
            api::Refused::new("a sandbox is given no API key, so it doesn't run there").io(),
        );
    }
    let out = crate::state_reads::append(&job.turn_file(n, TurnFile::Jsonl))?;
    let mut c = Command::new(&cfg.tools.timeout);
    // No claude.ai connectors (mail, calendar, drive, design tools): one of
    // 2,188 jobs in 14 days called one, and their tools were a ninth of
    // every job's context. The machine's own MCP servers stay.
    c.args(["-k", "1m", "0"])
        .stdout(out)
        .env("ENABLE_CLAUDEAI_MCP_SERVERS", "false");
    if let Some(agent) = agent {
        let next = if must {
            format!("Before answering, run {NEXT}.")
        } else {
            "If you are waiting on GitHub, run `proj await URL` for each target before answering."
                .into()
        };
        args.extend(["--append-system-prompt".into(), format!(
            "This is a sandboxed proj job, running headless. {next} The sandbox's proj records your declaration locally; the host registers GitHub waits when your run ends. Anything left running is stopped with the VM at the end of the turn. {LONG} {FACTS}"
        )]);
        c.args(["agent", "sh", agent, "env"])
            .arg(format!("PROJ_VM_NEXT={}", crate::guest::path(job, n)))
            .args([
                "sh",
                "-c",
                "rm -f -- \"$PROJ_VM_NEXT\"; exec claude \"$@\"",
                "sh",
            ])
            .args(&args)
            .stdin(crate::state_reads::open(prompt)?);
        return job.start_run(n, crate::now(), || c.spawn());
    }
    // Every run on the credit, its first and each retry, only once it's
    // admitted, with what it has room for as its budget (engine/api.rs).
    let admission = match choice.engine {
        Engine::Claude => None,
        Engine::ClaudeApi => {
            api::key(cfg).map_err(api::Refused::io)?;
            let admitted = api::admit(cfg, crate::now()).map_err(api::Refused::io)?;
            args.extend(admitted.flag());
            Some(admitted)
        }
    };
    let id = job.id;
    let scratch = scratch::dir(cfg, id);
    // Where its own worktrees go, and how long they last.
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
    let instructions = format!("{instructions} {FACTS}{split}");
    args.extend(["--append-system-prompt".into(), instructions]);
    // A login shell, so the job sees the same PATH and env as an SSH login,
    // and then its App's env, which nothing the login sets can undo; Claude
    // starts only once that has.
    c.stdin(crate::state_reads::open(prompt)?)
        .arg(&cfg.tools.bash)
        .args([
            "-lc",
            r#"eval "$PROJ_APP_ENV" && unset PROJ_APP_ENV && exec "$@""#,
        ])
        .arg("proj-engine")
        .arg(&cfg.me)
        .arg(format!("_{}", choice.engine.name()))
        .args(&args)
        .env("PROJ_PROBE_SOCKET", crate::probe::socket(cfg, job, n))
        .env(
            "PROJ_HOST_JOBS_SOCKET",
            crate::host_jobs::socket(cfg, job, n),
        )
        .env("PROJ_JOB", id)
        .env("PROJ_GITHUB_DIR", job.file(JobFile::Github))
        .env(
            "PROJ_GITHUB_KICK",
            if cfg.kick.is_empty() {
                String::new()
            } else {
                format!("{}/proj-watch", cfg.kick)
            },
        )
        .env(
            "PROJ_GITHUB_REFUSALS",
            job.turn_file(n, TurnFile::GithubRefusals),
        )
        .env("PROJ_WAIT_DIR", waitdir)
        .env("PROJ_SCRATCH", &scratch)
        .env("PROJ_APP_ENV", app_env);
    // The marker, with the budget, is written before the admission's lock
    // goes: the next admission's meter counts this run at it.
    let budget = admission.as_ref().map(|a| a.budget);
    let child = job.start_budgeted_run(n, crate::now(), choice.engine, budget, || c.spawn());
    drop(admission);
    child
}

/// Resolve the same job contract in Claude's final login/App environment,
/// with `engine`'s credentials and no other's (engine/api.rs).
pub fn exec_claude(cfg: &Config, engine: Engine, args: &[String]) -> Result<(), crate::Fail> {
    use std::os::unix::process::CommandExt;
    let id = std::env::var("PROJ_JOB").map_err(|e| crate::Fail::new(1, e.to_string()))?;
    let capabilities = super::capabilities::resolve(
        cfg,
        &Job::new(cfg, &id),
        &scratch::dir(cfg, &id),
        &|name| std::env::var_os(name),
        &machine::Machine::real(),
    )
    .map_err(|e| crate::Fail::new(1, e.to_string()))?;
    let mut command = Command::new("claude");
    if !capabilities.inherits_environment() {
        command.env_clear();
    }
    command
        .args(capabilities.claude_flags())
        .args(args)
        .envs(capabilities.environment());
    let _key = api::credentials(&mut command, engine, cfg)
        .map_err(|e| crate::Fail::new(1, e.to_string()))?;
    let error = command.exec();
    Err(crate::Fail::new(
        1,
        format!("couldn't start claude: {error}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_start_orders_the_marker_before_immediate_output() {
        use std::fs;
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = tmp.path().join("jobs");
        cfg.scratch = tmp.path().join("scratch");
        cfg.tools.timeout = tmp.path().join("immediate-child");
        let job = Job::new(&cfg, "job");
        fs::create_dir_all(&job.dir).unwrap();
        let prompt = job.path("prompt.1");
        fs::write(&prompt, "prompt").unwrap();
        let stream = job.turn("1", "jsonl");
        fs::write(&stream, "").unwrap();
        let fixture = tmp.path().join("immediate-child");
        // Stand in for timeout at the common process boundary, including the
        // VM command. It emits immediately without buffering or initialization.
        burrow_test::executable(
            &fixture,
            "#!/bin/sh\ntest \"$(cat)\" = prompt || exit 41\nprintf '%s\\n' '{\"type\":\"result\"}'\n",
        );
        for agent in [None, Some("vm")] {
            for _ in 0..2 {
                let mut child = spawn(
                    &cfg,
                    &job,
                    "1",
                    &prompt,
                    &job.path("waits"),
                    agent,
                    false,
                    "",
                    &Choice::default(),
                )
                .unwrap();
                assert!(child.wait().unwrap().success());
            }
        }
        let lines = fs::read_to_string(&stream).unwrap();
        let kinds: Vec<_> = lines
            .lines()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(line).unwrap()["type"]
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(kinds, ["burrow.run", "result"].repeat(4));
        fs::remove_file(job.turn("1", "started")).unwrap();
        fs::write(&stream, "").unwrap();
        cfg.tools.timeout = tmp.path().join("missing-engine");
        let job = Job::new(&cfg, "job");
        for agent in [None, Some("vm")] {
            assert!(
                spawn(
                    &cfg,
                    &job,
                    "1",
                    &prompt,
                    &job.path("waits"),
                    agent,
                    false,
                    "",
                    &Choice::default()
                )
                .is_err()
            );
            assert!(!job.turn("1", "started").exists());
            assert_eq!(fs::read_to_string(&stream).unwrap(), "");
        }
    }

    #[test]
    fn a_claude_turn_takes_its_model_and_effort() {
        let c = Choice::default();
        let plain = args(&c, None);
        assert!(!plain.contains(&"--model".into()) && !plain.contains(&"--effort".into()));
        let c = Choice {
            model: Some("chosen".into()),
            effort: Some("medium".into()),
            ..c
        };
        let given = args(&c, Some("session".into())).join(" ");
        assert!(
            given.contains("--effort medium") && given.contains("--model chosen"),
            "{given}"
        );
        assert!(given.contains("--resume session"), "{given}");
    }
}
