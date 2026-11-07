//! Engine argv and environment, inside the turn's timeout and login shell.

use super::{Choice, Engine, stream::resume};
use crate::{config::Config, job::Job, read, scratch};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// How a job waits on its own long work, which its answer would stop.
pub const LONG: &str = "A command that can outlast one Bash call (ten minutes), a long build or test run, goes to the background with its output and exit code written to a file, and you wait for that file in the foreground, in steps under ten minutes each; answer only once it's there.";

/// How a turn says what comes next: what resumes the job, if anything.
pub const NEXT: &str = "`proj await URL` for each PR or issue you're still waiting on (the job is resumed, with this conversation, once one of them changes: checks finish, a review, comment or push, merged or closed), `proj ask` when your answer is a question only a person can settle (their reply resumes it), or `proj done` when the work is finished";

/// Parent exec flags precede resume: its subcommand accepts model and JSON,
/// but not --sandbox or --add-dir (pinned CLI's exec/resume help).
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
        // no_new_privs in Codex's sandbox prevents sudo from elevating.
        Engine::Codex => ["--no-daemon", "exec", "--sandbox", "workspace-write"]
            .map(String::from)
            .to_vec(),
    };
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

/// Explicit git metadata roots override workspace-write's automatic .git
/// protection; the common dir is needed by worktrees created in scratch.
/// A project that isn't a repository yet gets its own `.git` granted
/// without making it, so the job can `git init` there (the pinned CLI
/// grants a path that doesn't exist yet), and Codex is told to start
/// outside one. Returns the exec flags that go before `resume`.
fn configure_codex(
    cfg: &Config,
    job: &Job,
    scratch: &Path,
    instructions: &str,
    home: &Path,
    args: &mut Vec<String>,
) -> std::io::Result<Vec<String>> {
    let project = PathBuf::from(read(&job.path("project")).unwrap_or_default());
    // proj await/done and commands' wait files write only this job's state.
    let mut roots = vec![project.clone(), scratch.to_owned(), job.dir.clone()];
    let git = Command::new(&cfg.tools.git)
        .arg("-C")
        .arg(&project)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()?;
    let (metadata, exec) = if git.status.success() {
        let common = String::from_utf8_lossy(&git.stdout).trim().to_owned();
        (PathBuf::from(common), Vec::new())
    } else {
        (
            project.join(".git"),
            vec!["--skip-git-repo-check".to_owned()],
        )
    };
    for (var, default) in [
        ("CARGO_HOME", home.join(".cargo")),
        ("XDG_CACHE_HOME", home.join(".cache")),
    ] {
        let root = std::env::var_os(var).map(PathBuf::from).unwrap_or(default);
        std::fs::create_dir_all(&root)?;
        roots.push(if var == "XDG_CACHE_HOME" {
            root.join("nix")
        } else {
            root
        });
    }
    for root in &roots {
        std::fs::create_dir_all(root)?;
    }
    roots.push(metadata);
    let quoted = |s: &str| serde_json::to_string(s).expect("string");
    let roots = roots
        .iter()
        .map(|p| format!("{}=\"write\"", quoted(&p.to_string_lossy())))
        .collect::<Vec<_>>()
        .join(",");
    for config in [
        r#"approval_policy="never""#.to_owned(),
        // Named permissions allow an explicit .git write grant; the legacy
        // workspace roots also insert protected metadata paths. Keep the
        // workspace-write mode, but name every grant in this profile.
        r#"default_permissions="proj""#.to_owned(),
        format!("permissions.proj.filesystem={{\":root\"=\"read\",\"/tmp\"=\"write\",{roots}}}"),
        "permissions.proj.network.enabled=true".to_owned(),
        // The login shell's App env and git hook config must reach commands.
        r#"shell_environment_policy.inherit="all""#.to_owned(),
        "shell_environment_policy.ignore_default_excludes=true".to_owned(),
        format!("developer_instructions={}", quoted(instructions)),
    ] {
        args.extend(["-c".into(), config]);
    }
    Ok(exec)
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
    let out = File::options().append(true).open(job.turn(n, "jsonl"))?;
    let mut c = Command::new(&cfg.tools.timeout);
    // No claude.ai connectors (mail, calendar, drive, design tools): one of
    // 2,188 jobs in 14 days called one, and their tools were a ninth of
    // every job's context. The machine's own MCP servers stay.
    c.args(["-k", "1m", "0"]).stdout(out);
    if choice.engine == Engine::Claude {
        c.env("ENABLE_CLAUDEAI_MCP_SERVERS", "false");
    } else {
        // Login/config failures may precede the first JSON event.
        c.stderr(File::create(job.turn(n, "stderr"))?);
    }
    if let Some(agent) = agent {
        c.args(["agent", "sh", agent, "claude"])
            .args(&args)
            .stdin(File::open(prompt)?);
        return c.spawn();
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
    if choice.engine == Engine::Claude {
        args.extend(["--append-system-prompt".into(), instructions]);
    } else {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| std::io::Error::other("HOME isn't set"))?;
        let exec = configure_codex(cfg, job, &scratch, &instructions, &home, &mut args)?;
        exec_flags(&mut args, exec);
        args.push("-".into());
    }
    // A login shell, so the job sees the same PATH and env as an SSH login,
    // and then its App's env, which nothing the login sets can undo; Claude
    // starts only once that has.
    c.arg(&cfg.tools.bash)
        .args([
            "-lc",
            r#"eval "$PROJ_APP_ENV" && unset PROJ_APP_ENV && exec "$@" <"$0""#,
        ])
        .arg(prompt);
    if choice.engine == Engine::Codex {
        // After the login and App env: the App sets its own git config count.
        c.arg(&cfg.me).arg("_codex");
    } else {
        c.arg("claude");
    }
    c.args(&args)
        .env("PROJ_JOB", id)
        .env("PROJ_WAIT_DIR", waitdir)
        .env("PROJ_SCRATCH", &scratch)
        .env("PROJ_APP_ENV", app_env)
        .stdin(Stdio::null())
        .spawn()
}

/// The login shell has applied the App's env. Append this job's hooks to
/// its git configuration now, preserving the App's credentials and rewrites.
pub fn codex_command(
    cfg: &Config,
    args: &[String],
    count: Option<&str>,
) -> Result<Command, String> {
    let count: usize = count
        .unwrap_or("0")
        .parse()
        .map_err(|_| "GIT_CONFIG_COUNT isn't a number")?;
    let mut c = Command::new(&cfg.tools.codex);
    c.args(args)
        .env("GIT_CONFIG_COUNT", (count + 1).to_string())
        .env(format!("GIT_CONFIG_KEY_{count}"), "core.hooksPath")
        .env(format!("GIT_CONFIG_VALUE_{count}"), &cfg.codex_hooks);
    Ok(c)
}
pub fn exec_codex(cfg: &Config, args: &[String]) -> Result<(), crate::Fail> {
    use std::os::unix::process::CommandExt;
    let count = std::env::var("GIT_CONFIG_COUNT").ok();
    let mut command =
        codex_command(cfg, args, count.as_deref()).map_err(|e| crate::Fail::new(1, e))?;
    Err(crate::Fail::new(
        1,
        format!("couldn't start codex: {}", command.exec()),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sandbox_and_resume_flags_are_in_the_parent_command() {
        let c = Choice {
            engine: Engine::Codex,
            model: None,
        };
        assert_eq!(
            args(&c, None),
            [
                "--no-daemon",
                "exec",
                "--sandbox",
                "workspace-write",
                "--json"
            ]
        );
        assert_eq!(
            args(&c, Some("thread".into())),
            [
                "--no-daemon",
                "exec",
                "--sandbox",
                "workspace-write",
                "resume",
                "--json",
                "thread"
            ]
        );
        let c = Choice {
            model: Some("chosen".into()),
            ..c
        };
        assert!(args(&c, None).ends_with(&["-m".into(), "chosen".into()]));
    }
    #[test]
    fn app_git_configuration_is_extended_after_the_login_shell() {
        let cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        let command = codex_command(&cfg, &["exec".into(), "--json".into()], Some("9")).unwrap();
        let env: std::collections::BTreeMap<_, _> = command
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap(), v.unwrap().to_str().unwrap()))
            .collect();
        assert_eq!(env["GIT_CONFIG_COUNT"], "10");
        assert_eq!(env["GIT_CONFIG_KEY_9"], "core.hooksPath");
        assert_eq!(env["GIT_CONFIG_VALUE_9"], "/hooks");
        assert!(!env.contains_key("GIT_CONFIG_KEY_0"));
        assert!(codex_command(&cfg, &[], Some("bad")).is_err());
    }

    #[test]
    fn sandbox_grants_are_exact_and_git_metadata_is_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.tools.git = "git".into();
        let project = dir.path().join("project");
        assert!(
            Command::new("git")
                .arg("init")
                .arg(&project)
                .status()
                .unwrap()
                .success()
        );
        let job = Job::new(&cfg, "job");
        std::fs::create_dir_all(&job.dir).unwrap();
        std::fs::write(job.path("project"), project.to_str().unwrap()).unwrap();
        let mut args = Vec::new();
        let exec = configure_codex(
            &cfg,
            &job,
            &dir.path().join("scratch"),
            "This is proj job job",
            &dir.path().join("home"),
            &mut args,
        )
        .unwrap();
        assert!(exec.is_empty());
        let config = args.join("\n");
        assert!(config.contains("default_permissions=\"proj\""));
        assert!(config.contains("permissions.proj.network.enabled=true"));
        assert!(config.contains(&format!("\"{}\"=\"write\"", project.join(".git").display())));
        assert!(config.contains("developer_instructions=\"This is proj job job\""));
        assert!(config.contains(&format!("\"{}\"=\"write\"", job.dir.display())));
    }

    #[test]
    fn a_project_with_no_repository_starts_and_may_make_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.tools.git = "git".into();
        let project = dir.path().join("plain");
        std::fs::create_dir_all(&project).unwrap();
        let job = Job::new(&cfg, "job");
        std::fs::create_dir_all(&job.dir).unwrap();
        std::fs::write(job.path("project"), project.to_str().unwrap()).unwrap();
        let mut args = Vec::new();
        let exec = configure_codex(
            &cfg,
            &job,
            &dir.path().join("scratch"),
            "This is proj job job",
            &dir.path().join("home"),
            &mut args,
        )
        .unwrap();
        assert_eq!(exec, ["--skip-git-repo-check"]);
        // Its .git is granted, for a `git init`, but not made: an empty one
        // would read as a broken repository.
        let config = args.join("\n");
        assert!(config.contains(&format!("\"{}\"=\"write\"", project.join(".git").display())));
        assert!(!project.join(".git").exists());
    }

    #[test]
    fn exec_flags_go_before_resume() {
        let c = Choice {
            engine: Engine::Codex,
            model: None,
        };
        let mut args = args(&c, Some("thread".into()));
        exec_flags(&mut args, vec!["--skip-git-repo-check".to_owned()]);
        assert_eq!(
            args,
            [
                "--no-daemon",
                "exec",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "resume",
                "--json",
                "thread"
            ]
        );
    }
}
