//! `burrow-agent CONFIG [WORD ARGS...]`, as `agent` (lib.rs): what makes,
//! stops or deletes a sandbox runs as root, through sudo; what reaches into
//! one, or fetches its work, runs as the user.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::IsTerminal;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{ExitCode, Stdio};

use burrow_agent::list::{self, header, line};
use burrow_agent::new;
use burrow_agent::sandbox::{self, Sandbox};
use burrow_agent::save::{Saved, save};
use burrow_agent::{Agent, Config, exit_code, mtime, now, ssh, touch, write_line};

const USAGE: &str = "usage: agent new|claude|sh|ls|fetch|path|stop|start|rm|keep|gc";

/// Why `agent` stops: what it says, and how it exits.
enum Stop {
    Usage(String),
    Failed(String),
    Code(u8),
}

impl From<String> for Stop {
    fn from(e: String) -> Stop {
        Stop::Failed(e)
    }
}

fn main() -> ExitCode {
    let args: Option<Vec<String>> = env::args_os()
        .skip(1)
        .map(OsString::into_string)
        .map(Result::ok)
        .collect();
    let Some(args) = args else {
        eprintln!("agent: its arguments must be UTF-8");
        return ExitCode::from(2);
    };
    let Some((config, words)) = args.split_first() else {
        eprintln!(
            "usage: burrow-agent CONFIG [new|claude|sh|ls|fetch|path|stop|start|rm|keep|gc ...]"
        );
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("agent: {e}");
            return ExitCode::from(2);
        }
    };
    let agent = match Agent::new(&cfg, Path::new(config)) {
        Ok(agent) => agent,
        Err(e) => {
            eprintln!("agent: {e}");
            return ExitCode::FAILURE;
        }
    };
    match run(&agent, words) {
        Ok(()) => ExitCode::SUCCESS,
        Err(Stop::Usage(u)) => {
            eprintln!("{u}");
            ExitCode::from(2)
        }
        Err(Stop::Failed(e)) => {
            // Empty when whatever failed has said why itself.
            if !e.is_empty() {
                eprintln!("agent: {e}");
            }
            ExitCode::FAILURE
        }
        Err(Stop::Code(n)) => ExitCode::from(n),
    }
}

fn run(agent: &Agent, w: &[String]) -> Result<(), Stop> {
    let cfg = agent.cfg;
    let word = w.first().map_or("ls", String::as_str);
    // The sandbox a word is about: its second word.
    let id = |usage: &str| {
        w.get(1)
            .filter(|id| !id.is_empty())
            .cloned()
            .ok_or_else(|| Stop::Usage(format!("usage: agent {usage}")))
    };
    match word {
        "new" => {
            agent.as_user(w)?;
            let id = new::new(agent, &w[1..])?;
            println!("{id}");
        }
        "_new" => {
            agent.as_root(w)?;
            let cwd = env::current_dir().map_err(|e| format!("the current directory: {e}"))?;
            let ask = match new::ask(cfg, &w[1..], &cwd) {
                Err(u) if u == new::usage(cfg) => return Err(Stop::Usage(u)),
                ask => ask?,
            };
            let id = new::make(agent, &ask)?;
            println!("{id}");
        }
        "claude" => {
            agent.as_user(w)?;
            let id = id("claude ID [ARGS...]")?;
            let mut command = vec!["claude".to_owned()];
            command.extend(w[2..].iter().cloned());
            return shell(agent, &id, &command);
        }
        "sh" => {
            agent.as_user(w)?;
            let id = id("sh ID [COMMAND...]")?;
            return shell(agent, &id, &w[2..]);
        }
        "ls" => ls(agent),
        "path" => {
            let sb = Sandbox::find(cfg, &id("path ID")?)?;
            println!("{}", sb.clone_path(cfg)?.display());
        }
        "fetch" => {
            agent.as_user(w)?;
            let sb = Sandbox::find(cfg, &id("fetch ID")?)?;
            if save(agent, &sb)? == Saved::NotARepository {
                eprintln!(
                    "agent: {} is not a git repository of its own; nothing to fetch into",
                    sb.project()?
                );
                return Err(Stop::Code(2));
            }
            let listed = agent
                .command("git")
                .arg("-C")
                .arg(cfg.src.join(sb.project()?))
                .args([
                    "for-each-ref",
                    "--format=%(refname) %(objectname:short) %(subject)",
                    &format!("refs/agents/{}/", sb.id),
                ])
                .status()
                .map_err(|e| format!("git: {e}"))?;
            if !listed.success() {
                return Err(Stop::Code(exit_code(listed).clamp(1, 255) as u8));
            }
        }
        "start" => {
            agent.as_root(w)?;
            let sb = Sandbox::find(cfg, &id("start ID")?)?;
            remove(&sb.file(sandbox::REMOVED))?;
            touch(&sb.file(sandbox::LAST_USED))?;
            sb.write_args(agent)?;
            agent.run("systemctl", &["start", &sb.unit()])?;
            let waited = agent
                .as_user_command(&["_wait", &sb.id])
                .status()
                .map_err(|e| format!("runuser: {e}"))?;
            if !waited.success() {
                return Err(Stop::Code(exit_code(waited).clamp(1, 255) as u8));
            }
        }
        "_wait" => {
            agent.as_user(w)?;
            let sb = Sandbox::find(cfg, &id("_wait ID")?)?;
            ssh::wait_ready(agent, &sb)?;
        }
        "stop" => {
            agent.as_root(w)?;
            let sb = Sandbox::find(cfg, &id("stop ID")?)?;
            stop_vm(agent, &sb)?;
        }
        "rm" => {
            agent.as_root(w)?;
            let sb = Sandbox::find(cfg, &id("rm ID")?)?;
            stop_vm(agent, &sb)?;
            let saved = list::saved_or_nowhere(fetched(agent, &sb));
            touch(&sb.file(sandbox::REMOVED))?;
            if saved {
                println!(
                    "{} stopped; deleted in a day unless you run: agent keep {}",
                    sb.id, sb.id
                );
            } else {
                eprintln!(
                    "{} stopped, but its work COULDN'T BE SAVED (agent fetch {} says why).",
                    sb.id, sb.id
                );
                eprintln!(
                    "It's deleted in a day with that work unless you run: agent keep {}",
                    sb.id
                );
            }
        }
        "keep" => {
            let sb = Sandbox::find(cfg, &id("keep ID [DAYS]")?)?;
            let given = w.get(2).map(String::as_str);
            let Some(days) = list::days(given) else {
                return Err(Stop::Usage(format!(
                    "usage: agent keep ID [DAYS]: DAYS is a whole number, not {}",
                    given.unwrap_or("")
                )));
            };
            remove(&sb.file(sandbox::REMOVED))?;
            write_line(&sb.file(sandbox::TTL_DAYS), &days.to_string())?;
            touch(&sb.file(sandbox::LAST_USED))?;
        }
        "gc" => {
            agent.as_root(w)?;
            return gc(agent);
        }
        _ => return Err(Stop::Usage(USAGE.to_owned())),
    }
    Ok(())
}

/// `command` in the sandbox, in its project: a login shell when there's
/// none. A stopped sandbox (by gc, a reboot, or agent stop) is started
/// first.
fn shell(agent: &Agent, id: &str, command: &[String]) -> Result<(), Stop> {
    let cfg = agent.cfg;
    let sb = Sandbox::find(cfg, id)?;
    if !running(agent, &sb) {
        let started = agent
            .as_root_command(&["start", &sb.id])
            .status()
            .map_err(|e| format!("{}: {e}", cfg.sudo.display()))?;
        if !started.success() {
            return Err(Stop::Code(exit_code(started).clamp(1, 255) as u8));
        }
    }
    touch(&sb.file(sandbox::LAST_USED))?;
    let line = ssh::remote(&cfg.src.join(sb.project()?), command);
    let tty: &[&str] = if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        &["-t"]
    } else {
        &[]
    };
    let e = ssh::ssh(agent, &sb, tty, &[line])?.exec();
    Err(format!("ssh: {e}").into())
}

fn running(agent: &Agent, sb: &Sandbox) -> bool {
    agent.quietly("systemctl", &["-q", "is-active", &sb.unit()])
}

/// The VM, and the virtiofsd's serving it, which otherwise outlive it and
/// keep its directory busy.
fn stop_vm(agent: &Agent, sb: &Sandbox) -> Result<(), String> {
    agent.run(
        "systemctl",
        &[
            "stop",
            &sb.unit(),
            &format!("microvm-virtiofsd@{}.service", sb.id),
            &format!("agent-shares@{}.service", sb.id),
        ],
    )
}

/// How `agent fetch`, as the user, exited: 0 when it saved the sandbox's
/// work, 2 when there's nowhere to save it (the project isn't a repository
/// of its own).
fn fetched(agent: &Agent, sb: &Sandbox) -> i32 {
    match agent
        .as_user_command(&["fetch", &sb.id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) => exit_code(s),
        Err(e) => {
            eprintln!("agent: runuser: {e}");
            1
        }
    }
}

fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

fn ls(agent: &Agent) {
    let cfg = agent.cfg;
    let now = now() as i64;
    println!("{}", header());
    for sb in Sandbox::all(cfg) {
        let state = if running(agent, &sb) {
            "running"
        } else if sb.removed() {
            "removed"
        } else {
            "stopped"
        };
        let used = agent
            .command("zfs")
            .args(["get", "-Hp", "-o", "value", "used"])
            .arg(format!("{}/{}", cfg.dataset, sb.id))
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .map_or("?".to_owned(), list::iec);
        let project = sb.project().unwrap_or_else(|_| "?".to_owned());
        match sb.expires() {
            Ok(at) => println!("{}", line(&sb.id, state, &project, &used, at - now)),
            Err(e) => eprintln!("agent: {}: {e}", sb.id),
        }
    }
}

/// Stops the sandboxes nobody has used for a day (and the removed ones),
/// unless someone is connected to them right now; saves and deletes the
/// stopped ones past their expiry. One it can't save is kept, and one it
/// can't see to is said, and the rest are still seen to.
fn gc(agent: &Agent) -> Result<(), Stop> {
    let now = now() as i64;
    let mut failed = false;
    for sb in Sandbox::all(agent.cfg) {
        if let Err(e) = gc_one(agent, &sb, now) {
            eprintln!("agent: {}: {e}", sb.id);
            failed = true;
        }
    }
    if failed {
        Err(Stop::Failed(String::new()))
    } else {
        Ok(())
    }
}

fn gc_one(agent: &Agent, sb: &Sandbox, now: i64) -> Result<(), String> {
    let cfg = agent.cfg;
    if running(agent, sb) {
        let idle = now - mtime(&sb.file(sandbox::LAST_USED))? as i64;
        if list::in_use(sb.cid()?) {
            return Ok(());
        }
        if list::stops(sb.removed(), idle) {
            println!("{}: stopping (unused for {}h)", sb.id, idle / 3600);
            stop_vm(agent, sb)?;
        }
        return Ok(());
    }
    if now < sb.expires()? {
        return Ok(());
    }
    let rc = fetched(agent, sb);
    if list::deletes(rc, sb.removed()) {
        println!(
            "{}: work saved to refs/agents/{} in {}; deleting",
            sb.id,
            sb.id,
            sb.project()?
        );
        stop_vm(agent, sb)?;
        agent.run(
            "zfs",
            &["destroy", "-r", &format!("{}/{}", cfg.dataset, sb.id)],
        )?;
        agent.run(
            "zfs",
            &["destroy", &format!("{}@agent-{}", cfg.data_dataset, sb.id)],
        )?;
        // zfs usually removes the mountpoint itself.
        if sb.dir.is_dir() {
            fs::remove_dir(&sb.dir).map_err(|e| format!("{}: {e}", sb.dir.display()))?;
        }
    } else {
        eprintln!(
            "{}: expired, but its work couldn't be saved (fetch exit {rc}); keeping it",
            sb.id
        );
    }
    Ok(())
}
