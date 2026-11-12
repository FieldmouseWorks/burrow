//! Engine argv and environment, inside the turn's timeout and login shell.

use super::sandbox::{self, Env};
use super::{Choice, Engine, stream::resume};
use crate::{config::Config, job::Job, scratch};

use std::path::{Path, PathBuf};
use std::process::{Child, Command};

/// How a job waits on its own long work, which its answer would stop.
pub const LONG: &str = "A command that can outlast one Bash call (ten minutes), a long build or test run, goes to the background with its output and exit code written to a file, and you wait for that file in the foreground, in steps under ten minutes each; answer only once it's there.";

/// How a turn says what comes next: what resumes the job, if anything.
pub const NEXT: &str = "`proj await URL` for each PR or issue you're still waiting on (the job is resumed, with this conversation, once one of them changes: checks finish, a review, comment or push, merged or closed), `proj ask --default \"TEXT\" [--one-way]` when your answer is a question only a person can settle (their reply resumes it; after the deadline a reversible default is taken, a one-way default is left for the owner), or `proj done` when the work is finished";

pub const FACTS: &str = "Check machine facts with `proj probe QUESTION...` before asking anyone; jobs never open probe issues.";
const SPLIT: &str = " An inbox task is one self-contained PR. If it is too big, publish progress and write 2 to 16 smaller self-contained tasks as a JSON array with title, body and check, run `proj split FILE`, then end the session. Preserve the original authorization boundaries, outstanding work and PR links in those tasks; never carry a lane indefinitely.";

/// Parent exec flags precede resume: its subcommand accepts model and JSON,
/// but not the parent's own flags (pinned CLI's exec/resume help).
fn args(choice: &Choice, session: Option<String>) -> Vec<String> {
    let mut args: Vec<String> = match choice.engine {
        Engine::Claude => [
            "-p",
            "--output-format",
            "stream-json",
            "--verbose",
            "--disable-slash-commands",
        ]
        .map(String::from)
        .to_vec(),
        // Stay in the turn's unit, rather than a shared background daemon.
        // No --sandbox: the pinned CLI reads that flag as the legacy policy
        // and drops the named profile `proj _codex` gives (its
        // resolve_permission_config_syntax), leaving only the cwd and /tmp
        // writable, .git read-only and the network, unix sockets included,
        // refused. The profile is the whole sandbox.
        Engine::Codex => ["--no-daemon", "exec"].map(String::from).to_vec(),
    };
    if let Some(e) = &choice.effort {
        // Codex's is exec's own config, so it goes before resume too.
        match choice.engine {
            Engine::Claude => args.extend(["--effort".into(), e.clone()]),
            Engine::Codex => args.extend(["-c".into(), format!("model_reasoning_effort=\"{e}\"")]),
        }
    }
    if let Some(s) = session {
        match choice.engine {
            Engine::Claude => args.extend(["--resume".into(), s]),
            Engine::Codex => args.extend(["resume".into(), "--json".into(), s]),
        }
    } else if choice.engine == Engine::Codex {
        args.push("--json".into());
    }
    if let Some(m) = &choice.model {
        args.extend([
            if choice.engine == Engine::Claude {
                "--model"
            } else {
                "-m"
            }
            .into(),
            m.clone(),
        ]);
    }
    args
}

/// Puts `flags` of exec's own right after `exec`, so they come before
/// `resume`, whose subcommand doesn't take them.
fn exec_flags(args: &mut Vec<String>, flags: Vec<String>) {
    let at = args.iter().position(|a| a == "exec").map_or(0, |i| i + 1);
    args.splice(at..at, flags);
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
    let out = crate::state_reads::append(&job.turn(n, "jsonl"))?;
    let mut c = Command::new(&cfg.tools.timeout);
    // No claude.ai connectors (mail, calendar, drive, design tools): one of
    // 2,188 jobs in 14 days called one, and their tools were a ninth of
    // every job's context. The machine's own MCP servers stay.
    c.args(["-k", "1m", "0"]).stdout(out);
    if choice.engine == Engine::Claude {
        c.env("ENABLE_CLAUDEAI_MCP_SERVERS", "false");
    } else {
        // Login/config failures may precede the first JSON event.
        crate::state_reads::write(job.turn(n, "stderr"), "")?;
        c.stderr(crate::state_reads::append(&job.turn(n, "stderr"))?);
    }
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
    let split =
        if job.path("inbox-issue").exists() && cfg.inbox.as_ref().is_some_and(|i| i.planning) {
            SPLIT
        } else {
            ""
        };
    let instructions = format!("{instructions} {FACTS}{split}");
    if choice.engine == Engine::Claude {
        args.extend(["--append-system-prompt".into(), instructions]);
    } else {
        // The sandbox's own flags come once the login and the App's env have
        // set the environment Codex starts in (`codex_args`).
        let quoted = serde_json::to_string(&instructions).expect("string");
        args.extend(["-c".into(), format!("developer_instructions={quoted}")]);
        args.push("-".into());
    }
    // A login shell, so the job sees the same PATH and env as an SSH login,
    // and then its App's env, which nothing the login sets can undo; Claude
    // starts only once that has.
    c.stdin(crate::state_reads::open(prompt)?)
        .arg(&cfg.tools.bash)
        .args([
            "-lc",
            r#"eval "$PROJ_APP_ENV" && unset PROJ_APP_ENV && exec "$@""#,
        ])
        .arg("proj-engine");
    if choice.engine == Engine::Codex {
        // After the login and App env: the App sets its own git config count.
        c.arg(&cfg.me).arg("_codex");
    } else {
        c.arg(&cfg.me).arg("_claude");
    }
    c.args(&args)
        .env("PROJ_PROBE_SOCKET", crate::probe::socket(cfg, job, n))
        .env(
            "PROJ_HOST_JOBS_SOCKET",
            crate::host_jobs::socket(cfg, job, n),
        )
        .env("PROJ_JOB", id)
        .env("PROJ_GITHUB_DIR", job.path("github"))
        .env(
            "PROJ_GITHUB_KICK",
            if cfg.kick.is_empty() {
                String::new()
            } else {
                format!("{}/proj-watch", cfg.kick)
            },
        )
        .env("PROJ_GITHUB_REFUSALS", job.turn(n, "github-refusals"))
        .env("PROJ_WAIT_DIR", waitdir)
        .env("PROJ_SCRATCH", &scratch)
        .env("PROJ_APP_ENV", app_env);
    job.start_run(n, crate::now(), || c.spawn())
}

/// Codex's argv, the job's own /tmp it runs over, and the machine's paths
/// under /tmp kept there, and where the CODEX_HOME it was given leads.
pub struct Start {
    pub args: Vec<String>,
    pub tmp: PathBuf,
    pub kept: Vec<PathBuf>,
    pub codex_home: Option<PathBuf>,
}

/// The login shell has applied the App's env, so this is the environment
/// Codex starts in: the job's sandbox goes into its exec flags now
/// ([`sandbox`]), naming the paths Codex and the job's commands will find,
/// with the job's own /tmp, which Codex then runs over.
pub fn codex_args(
    cfg: &Config,
    args: &[String],
    env: Env,
    machine: &sandbox::Machine,
) -> Result<Start, String> {
    let id = env("PROJ_JOB")
        .and_then(|v| v.into_string().ok())
        .filter(|v| !v.is_empty())
        .ok_or("PROJ_JOB: proj _codex starts a proj job's Codex")?;
    let job = Job::new(cfg, &id);
    let scratch = scratch::dir(cfg, &id);
    let made = |e: std::io::Error| format!("couldn't make the job's sandbox: {e}");
    let sandbox = sandbox::flags(cfg, &job, &scratch, env, machine).map_err(made)?;
    let tmp = sandbox::private_tmp(&scratch).map_err(made)?;
    sandbox::check_kept(&tmp, machine, &sandbox.kept).map_err(made)?;
    let mut args = args.to_vec();
    exec_flags(&mut args, sandbox.flags);
    Ok(Start {
        args,
        tmp,
        kept: sandbox.kept,
        codex_home: sandbox.codex_home,
    })
}

/// The login shell has applied the App's env. Append this job's hooks to
/// its git configuration now, preserving the App's credentials and rewrites,
/// and start Codex over the job's own /tmp.
pub fn codex_command(cfg: &Config, start: &Start, count: Option<&str>) -> Result<Command, String> {
    let count: usize = count
        .unwrap_or("0")
        .parse()
        .map_err(|_| "GIT_CONFIG_COUNT isn't a number")?;
    let mut c = Command::new(&cfg.tools.bwrap);
    c.args(sandbox::wrap(
        &start.tmp,
        &start.kept,
        start.codex_home.as_deref(),
        &cfg.tools.codex,
    ))
    .args(&start.args)
    .env("GIT_CONFIG_COUNT", (count + 1).to_string())
    .env(format!("GIT_CONFIG_KEY_{count}"), "core.hooksPath")
    .env(format!("GIT_CONFIG_VALUE_{count}"), &cfg.codex_hooks);
    Ok(c)
}
/// Resolve the same job contract in Claude's final login/App environment.
pub fn exec_claude(cfg: &Config, args: &[String]) -> Result<(), crate::Fail> {
    use std::os::unix::process::CommandExt;
    let id = std::env::var("PROJ_JOB").map_err(|e| crate::Fail::new(1, e.to_string()))?;
    let capabilities = super::capabilities::resolve(
        cfg,
        &Job::new(cfg, &id),
        &scratch::dir(cfg, &id),
        &|name| std::env::var_os(name),
        &sandbox::Machine::real(),
    )
    .map_err(|e| crate::Fail::new(1, e.to_string()))?;
    let mut command = Command::new("claude");
    if !capabilities.inherits_environment() {
        command.env_clear();
    }
    let error = command
        .args(capabilities.claude_flags())
        .args(args)
        .envs(capabilities.environment())
        .exec();
    Err(crate::Fail::new(
        1,
        format!("couldn't start claude: {error}"),
    ))
}

pub fn exec_codex(cfg: &Config, args: &[String]) -> Result<(), crate::Fail> {
    use std::os::unix::process::CommandExt;
    let count = std::env::var("GIT_CONFIG_COUNT").ok();
    let machine = sandbox::Machine::real();
    let start = codex_args(cfg, args, &|name| std::env::var_os(name), &machine)
        .map_err(|e| crate::Fail::new(1, e))?;
    let mut command =
        codex_command(cfg, &start, count.as_deref()).map_err(|e| crate::Fail::new(1, e))?;
    Err(crate::Fail::new(
        1,
        format!("couldn't start codex: {}", command.exec()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_engine_and_vm_start_orders_the_marker_before_immediate_output() {
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
        for engine in [Engine::Claude, Engine::Codex] {
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
                        &Choice {
                            engine,
                            ..Choice::default()
                        },
                    )
                    .unwrap();
                    assert!(child.wait().unwrap().success());
                }
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
        assert_eq!(kinds, ["burrow.run", "result"].repeat(8));
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
    fn the_profile_alone_is_the_sandbox_and_resume_flags_are_in_the_parent_command() {
        let c = Choice {
            engine: Engine::Codex,
            ..Choice::default()
        };
        assert_eq!(args(&c, None), ["--no-daemon", "exec", "--json"]);
        assert_eq!(
            args(&c, Some("thread".into())),
            ["--no-daemon", "exec", "resume", "--json", "thread"]
        );
        let c = Choice {
            model: Some("chosen".into()),
            ..c
        };
        assert!(args(&c, None).ends_with(&["-m".into(), "chosen".into()]));
        let c = Choice {
            effort: Some("xhigh".into()),
            ..c
        };
        assert_eq!(
            args(&c, Some("thread".into()))[..4],
            [
                "--no-daemon",
                "exec",
                "-c",
                "model_reasoning_effort=\"xhigh\""
            ]
        );
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
    #[test]
    fn app_git_configuration_is_extended_after_the_login_shell() {
        let cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        let start = Start {
            args: vec!["exec".into(), "--json".into()],
            tmp: "/scratch/tmp".into(),
            kept: vec!["/tmp/codex".into()],
            codex_home: Some("/home/dev/codex".into()),
        };
        let command = codex_command(&cfg, &start, Some("9")).unwrap();
        // Codex, over the job's own /tmp with what it keeps of the
        // machine's, with its args after.
        assert_eq!(command.get_program(), "/bin/bwrap");
        let argv: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            argv[4..9],
            ["/scratch/tmp", "/tmp", "--bind", "/tmp/codex", "/tmp/codex"]
        );
        assert_eq!(
            argv[argv.len() - 4..],
            ["--", "/bin/codex", "exec", "--json"]
        );
        // With the CODEX_HOME it was given, where that leads.
        let home = argv.iter().position(|a| *a == "CODEX_HOME").unwrap();
        assert_eq!(
            argv[home - 1..=home + 1],
            ["--setenv", "CODEX_HOME", "/home/dev/codex"]
        );
        let env: std::collections::BTreeMap<_, _> = command
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap(), v.unwrap().to_str().unwrap()))
            .collect();
        assert_eq!(env["GIT_CONFIG_COUNT"], "10");
        assert_eq!(env["GIT_CONFIG_KEY_9"], "core.hooksPath");
        assert_eq!(env["GIT_CONFIG_VALUE_9"], "/hooks");
        assert!(!env.contains_key("GIT_CONFIG_KEY_0"));
        assert!(codex_command(&cfg, &start, Some("bad")).is_err());
    }

    #[test]
    fn the_sandbox_is_made_in_the_environment_codex_starts_in() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.scratch = dir.path().join("scratch");
        cfg.tools.git = "git".into();
        let job = Job::new(&cfg, "job");
        std::fs::create_dir_all(&job.dir).unwrap();
        let project = dir.path().join("plain");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(job.path("project"), project.to_str().unwrap()).unwrap();
        let machine = sandbox::tests::machine_in(dir.path());
        let home = dir.path().join("home");
        // Where the login shell's profile moved Codex's home to.
        let moved = dir.path().join("moved");
        std::fs::create_dir_all(&moved).unwrap();
        let env = |name: &str| match name {
            "PROJ_JOB" => Some("job".into()),
            "HOME" => Some(home.clone().into()),
            "CODEX_HOME" => Some(moved.clone().into()),
            _ => None,
        };
        let c = Choice {
            engine: Engine::Codex,
            ..Choice::default()
        };
        let mut given = args(&c, Some("thread".into()));
        given.push("-".into());
        let Start {
            args,
            tmp,
            kept,
            codex_home,
        } = codex_args(&cfg, &given, &env, &machine).unwrap();
        assert_eq!(tmp, cfg.scratch.join("jobs/job/tmp"));
        assert_eq!(codex_home, Some(moved.canonicalize().unwrap()));
        assert!(kept.is_empty(), "{kept:?}");
        let denied = |dir: &Path| format!("\"{}\"=\"deny\"", dir.display());
        let config = args.join("\n");
        assert!(
            config.contains(&denied(&moved.canonicalize().unwrap())),
            "{config}"
        );
        assert!(!config.contains(&denied(&home.join(".codex"))), "{config}");
        assert!(config.contains(&format!(
            "\"{}\"=\"write\"",
            cfg.scratch.join("jobs/job").display()
        )));
        // All of it is exec's, before resume; the prompt still comes last.
        assert_eq!(args[..3], ["--no-daemon", "exec", "--skip-git-repo-check"]);
        assert_eq!(args[args.len() - 4..], ["resume", "--json", "thread", "-"]);
        assert!(codex_args(&cfg, &given, &|_| None, &machine).is_err());
    }

    #[test]
    fn exec_flags_go_before_resume() {
        let c = Choice {
            engine: Engine::Codex,
            ..Choice::default()
        };
        let mut args = args(&c, Some("thread".into()));
        exec_flags(&mut args, vec!["--skip-git-repo-check".to_owned()]);
        assert_eq!(
            args,
            [
                "--no-daemon",
                "exec",
                "--skip-git-repo-check",
                "resume",
                "--json",
                "thread"
            ]
        );
    }
}
