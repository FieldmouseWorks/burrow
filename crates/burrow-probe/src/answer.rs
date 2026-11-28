//! Each question's answer, as plain text, read from the machine as it is:
//! files under /proc, /sys/fs/cgroup and /run, and a few commands, each with
//! fixed arguments and the unit names and times query.rs made. Nothing here
//! writes anything. What it says goes through the scrubber before it leaves
//! (pass.rs); here it's as the machine said it.

mod contains;
mod crashes;
mod hotspots;
mod memory;
mod system;
#[cfg(test)]
mod tests;
pub(crate) mod usage;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::Config;
use crate::query::Query;

/// How long one command may take before it's stopped and the question gets
/// that for its answer.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The most journal lines one question gets: the last ones of its span.
pub const JOURNAL_LINES: usize = 400;

/// Where the answers come from.
pub struct Machine<'a> {
    pub cfg: &'a Config,
    /// What /proc, /sys and /run are under: `/`, but in tests.
    pub root: PathBuf,
    /// The answer's time, in seconds since the epoch.
    pub now: i64,
    /// The user's uid, whose user manager and cgroup are read.
    pub uid: u32,
    pub run: &'a dyn Run,
}

/// A command run to its end.
#[derive(Debug, Default)]
pub struct Ran {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// A command run to its end, keeping raw stdout bytes.
#[derive(Debug, Default)]
pub struct RanBytes {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Runs a command: the real one, or a test's stand-in.
pub trait Run {
    /// `program args`, with the user manager's XDG_RUNTIME_DIR when `user`.
    fn run(&self, program: &Path, args: &[&str], user: bool, uid: u32) -> Result<Ran, String>;
    /// Read local commit objects with no inherited Git overrides.
    fn history(&self, program: &Path, args: &[&str], uid: u32) -> Result<Ran, String> {
        self.run(program, args, false, uid)
    }
    /// Read local git objects with stdin and raw stdout bytes, with no
    /// inherited Git overrides.
    fn history_bytes(
        &self,
        program: &Path,
        args: &[&str],
        stdin: &[u8],
        uid: u32,
    ) -> Result<RanBytes, String> {
        let _ = stdin;
        let r = self.history(program, args, uid)?;
        Ok(RanBytes {
            code: r.code,
            stdout: r.stdout.into_bytes(),
            stderr: r.stderr,
        })
    }
}

/// The real thing, stopped past COMMAND_TIMEOUT.
pub struct Commands;

impl Run for Commands {
    fn run(&self, program: &Path, args: &[&str], user: bool, uid: u32) -> Result<Ran, String> {
        command(program, args, user, uid, false)
    }
    fn history(&self, program: &Path, args: &[&str], uid: u32) -> Result<Ran, String> {
        command(program, args, false, uid, true)
    }
    fn history_bytes(
        &self,
        program: &Path,
        args: &[&str],
        stdin: &[u8],
        uid: u32,
    ) -> Result<RanBytes, String> {
        command_bytes(program, args, false, uid, true, Some(stdin))
    }
}
fn command(
    program: &Path,
    args: &[&str],
    user: bool,
    uid: u32,
    history: bool,
) -> Result<Ran, String> {
    let ran = command_bytes(program, args, user, uid, history, None)?;
    Ok(Ran {
        code: ran.code,
        stdout: String::from_utf8_lossy(&ran.stdout).into_owned(),
        stderr: ran.stderr,
    })
}

fn command_bytes(
    program: &Path,
    args: &[&str],
    user: bool,
    uid: u32,
    history: bool,
    stdin: Option<&[u8]>,
) -> Result<RanBytes, String> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("SYSTEMD_COLORS", "0")
        .env("SYSTEMD_PAGER", "");
    if history {
        for (key, _) in std::env::vars_os() {
            if key.as_encoded_bytes().starts_with(b"GIT_") {
                cmd.env_remove(key);
            }
        }
        cmd.env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("GIT_GRAFT_FILE", "/dev/null");
    }
    if user {
        cmd.env("XDG_RUNTIME_DIR", format!("/run/user/{uid}"));
    }
    let name = program.display();
    let mut child = cmd.spawn().map_err(|e| format!("{name}: {e}"))?;
    let writer = stdin.map(|input| {
        let data = input.to_vec();
        let mut pipe = child.stdin.take();
        thread::spawn(move || {
            if let Some(mut p) = pipe.take() {
                let _ = p.write_all(&data);
            }
        })
    });
    let out = {
        let mut p = child.stdout.take();
        thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(ref mut p) = p {
                let _ = p.read_to_end(&mut s);
            }
            s
        })
    };
    let err = {
        let mut p = child.stderr.take();
        thread::spawn(move || {
            let mut s = Vec::new();
            if let Some(ref mut p) = p {
                let _ = p.read_to_end(&mut s);
            }
            String::from_utf8_lossy(&s).into_owned()
        })
    };
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < COMMAND_TIMEOUT => {
                thread::sleep(Duration::from_millis(20))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                if let Some(w) = writer {
                    let _ = w.join();
                }
                return Err(format!(
                    "{name} took over {}s, and was stopped",
                    COMMAND_TIMEOUT.as_secs()
                ));
            }
            Err(e) => return Err(format!("{name}: {e}")),
        }
    };
    if let Some(w) = writer {
        let _ = w.join();
    }
    Ok(RanBytes {
        code: status.code(),
        stdout: out.join().unwrap_or_default(),
        stderr: err.join().unwrap_or_default(),
    })
}

impl Machine<'_> {
    /// The answer to `q`, or why there's none.
    pub fn answer(&self, q: &Query) -> Result<String, String> {
        match q {
            Query::Memory => memory::memory(self),
            Query::Usage { since, totals } => usage::answer(self, since.at(self.now), *totals),
            Query::Units => system::units(self),
            Query::Journal { unit, since, until } => {
                system::journal(self, unit, since.at(self.now), until.at(self.now))
            }
            Query::Queue => system::queue(self),
            Query::Why { issue } => system::why(self, issue),
            Query::Contains { repo, commit } => contains::answer(self, repo, commit),
            Query::Hotspots { repo, since } => hotspots::answer(self, repo, since.at(self.now)),
            Query::Deployed => system::deployed(self),
            Query::Oom { since } => system::oom(self, since.at(self.now)),
            Query::Crashes { exe, since } => crashes::answer(self, exe, since.at(self.now)),
        }
    }

    /// A path under the machine's root.
    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p.trim_start_matches('/'))
    }

    /// A command's stdout, or what it said when it failed. A code in `fine`
    /// with nothing on stderr is no failure (journalctl -g's 1 for nothing
    /// found, say).
    fn output(
        &self,
        program: &Path,
        args: &[&str],
        user: bool,
        fine: &[i32],
    ) -> Result<String, String> {
        let r = self.run.run(program, args, user, self.uid)?;
        match r.code {
            Some(0) => Ok(r.stdout),
            Some(c) if fine.contains(&c) && r.stderr.trim().is_empty() => Ok(r.stdout),
            code => {
                let why = r.stderr.trim();
                let how = code.map_or("was killed".to_string(), |c| format!("exited {c}"));
                Err(format!(
                    "{} {how}{}",
                    program.file_name().unwrap_or_default().to_string_lossy(),
                    if why.is_empty() {
                        String::new()
                    } else {
                        format!(": {why}")
                    }
                ))
            }
        }
    }
}

/// Bytes, as a person reads them: 12.3G, 512M, 64K.
pub fn size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b >= K * K * K {
        format!("{:.1}G", b / (K * K * K))
    } else if b >= K * K {
        format!("{:.0}M", b / (K * K))
    } else {
        format!("{:.0}K", b / K)
    }
}

/// An epoch's time in UTC, to the second.
pub fn utc(epoch: i64) -> String {
    jiff::Timestamp::from_second(epoch).map_or_else(
        |_| format!("@{epoch}"),
        |t| t.strftime("%Y-%m-%d %H:%M:%SZ").to_string(),
    )
}

/// Lines as they came, or `none` for none.
fn or_none(text: &str) -> String {
    let t = text.trim_end();
    if t.trim().is_empty() {
        "none".to_string()
    } else {
        t.to_string()
    }
}
