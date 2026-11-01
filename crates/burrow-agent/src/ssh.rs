//! Reaching a sandbox: ssh as the user, over vsock, and waiting for it to
//! come up.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::Agent;
use crate::sandbox::Sandbox;

/// How long sshd in a sandbox may take to send its banner: a while, on a
/// loaded machine. The vsock connects at once, so the banner's is
/// ConnectTimeout.
pub const BANNER: Duration = Duration::from_secs(60);
/// How long the sandbox may take to boot.
pub const BOOT: Duration = Duration::from_secs(900);

/// ssh into a sandbox as the user, with `opts` first (the first value ssh
/// is given for an option is the one it takes), then `command`.
pub fn ssh(
    agent: &Agent,
    sb: &Sandbox,
    opts: &[&str],
    command: &[String],
) -> Result<Command, String> {
    let cfg = agent.cfg;
    let cid = sb.cid()?;
    let mut c = agent.command("ssh");
    c.arg("-i")
        .arg(cfg.key())
        .args(opts)
        .args(["-o", "BatchMode=yes", "-o", "LogLevel=ERROR"])
        .arg("-o")
        .arg(format!("ConnectTimeout={}", BANNER.as_secs()))
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
        ])
        .arg("-o")
        .arg(format!("ProxyCommand=socat - VSOCK-CONNECT:{cid}:22"))
        .arg(format!("{}@agent-{cid}", cfg.user))
        .args(command);
    Ok(c)
}

/// What the sandbox's shell runs: into the project, then a login shell, or
/// `args` as one command line.
pub fn remote(project: &Path, args: &[String]) -> String {
    let cd = format!("cd {}", quote(&project.to_string_lossy()));
    if args.is_empty() {
        return format!("{cd} && exec bash -l");
    }
    let line: String = args.iter().map(|a| quote(a) + " ").collect();
    format!("{cd} && exec bash -lc {}", quote(&line))
}

/// `s` as one word to a POSIX shell: as it is when nothing in it is
/// special, else in single quotes.
pub fn quote(s: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_./=:,+@%-".contains(c);
    if !s.is_empty() && s.chars().all(plain) {
        return s.to_owned();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Runs `c`, its output gone, for `limit` at most: whether it succeeded
/// in time.
fn within(c: &mut Command, limit: Duration) -> bool {
    let Ok(mut child) = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let until = Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if Instant::now() < until => sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Waits for the sandbox's sshd, while its unit runs, for BOOT at most.
pub fn wait_ready(agent: &Agent, sb: &Sandbox) -> Result<(), String> {
    let limit = BANNER + Duration::from_secs(5);
    let until = Instant::now() + BOOT;
    let truth = ["true".to_owned()];
    while Instant::now() < until {
        if within(&mut ssh(agent, sb, &[], &truth)?, limit) {
            return Ok(());
        }
        if !agent.quietly("systemctl", &["-q", "is-active", &sb.unit()]) {
            break;
        }
        sleep(Duration::from_millis(500));
    }
    // Once more, out loud, so the reason is on screen.
    said_last(
        &mut ssh(agent, sb, &["-o", "LogLevel=DEBUG1"], &truth)?,
        limit,
        25,
    );
    Err(format!(
        "{} did not come up; see: journalctl -u {}",
        sb.id,
        sb.unit()
    ))
}

/// Runs `c` for `limit` at most, then puts the last `n` lines it said on
/// stderr.
fn said_last(c: &mut Command, limit: Duration, n: usize) {
    let Ok(mut child) = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return;
    };
    let mut err = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut text = Vec::new();
        if let Some(e) = err.as_mut() {
            let _ = e.read_to_end(&mut text);
        }
        text
    });
    let until = Instant::now() + limit;
    while matches!(child.try_wait(), Ok(None)) && Instant::now() < until {
        sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    let _ = child.wait();
    let text = reader.join().unwrap_or_default();
    let text = String::from_utf8_lossy(&text);
    let lines: Vec<&str> = text.lines().collect();
    for line in &lines[lines.len().saturating_sub(n)..] {
        eprintln!("{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_quoted_when_the_shell_would_change_it() {
        for (word, quoted) in [
            ("plain", "plain"),
            ("/data/dev/src/web", "/data/dev/src/web"),
            ("a=b,c:d@e%f+g-h", "a=b,c:d@e%f+g-h"),
            ("", "''"),
            ("two words", "'two words'"),
            ("it's", r"'it'\''s'"),
            ("$HOME", "'$HOME'"),
            ("a;b", "'a;b'"),
            ("*", "'*'"),
            ("~", "'~'"),
            ("x\ny", "'x\ny'"),
        ] {
            assert_eq!(quote(word), quoted, "{word:?}");
        }
    }

    #[test]
    fn the_remote_command_runs_in_the_project() {
        let p = Path::new("/data/dev/src/my web");
        assert_eq!(remote(p, &[]), "cd '/data/dev/src/my web' && exec bash -l");
        // Each argument reaches the command as it was, through both shells:
        // ssh's, then the login shell's -c.
        let args = [
            "printf",
            "%s|",
            "two words",
            "it's",
            "$HOME",
            "a;b",
            "*",
            "",
        ]
        .map(String::from);
        let line = remote(Path::new("/"), &args);
        let inner = line
            .strip_prefix("cd / && exec bash -lc ")
            .expect("into the project, then one command line");
        let sh = |script: &str| {
            let o = Command::new("/bin/sh")
                .args(["-c", script])
                .output()
                .unwrap();
            assert!(o.status.success(), "{script}");
            String::from_utf8(o.stdout).unwrap()
        };
        let once = sh(&format!("printf %s {inner}"));
        assert_eq!(sh(&once), "two words|it's|$HOME|a;b|*||");
    }
}
