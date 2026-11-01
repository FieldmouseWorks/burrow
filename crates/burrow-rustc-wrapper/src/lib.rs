//! The compile cache's way into every Rust build (modules/rust-cache.nix):
//!
//!   burrow-rustc-wrapper CONFIG COMPILER [ARGS...]
//!
//! cargo runs `RUSTC_WRAPPER RUSTC ARGS...`, and a build script's cc crate
//! `RUSTC_WRAPPER CC ARGS...`; RUSTC_WRAPPER's launcher runs this, the
//! running system's wrapper, with the compile. The compile runs through
//! sccache with the user's own cache, here, in the build's own process and
//! unit (sccache's client-side mode): the user's daemon, rust-cache-USER,
//! only reads and writes the cache. Whatever keeps the cache from being used
//! compiles without it instead, so a build never fails for the cache's sake.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use serde::Deserialize;

/// What modules/rust-cache.nix tells the wrapper, as JSON.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Each user's cache is ROOT/USER/cache, and the link to its daemon's
    /// socket ROOT/USER/sccache.sock.
    pub root: PathBuf,
    /// sccache's config, the same for the builds as for the daemons.
    pub conf: PathBuf,
    pub sccache: PathBuf,
    /// id, to name this user when USER doesn't.
    pub id: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// The compile itself, `compile[0]` run with the rest. `compile` is never
/// empty.
pub fn direct(compile: &[OsString]) -> Command {
    let mut run = Command::new(&compile[0]);
    run.args(&compile[1..]);
    run
}

/// How to run a compile.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    /// Without the cache.
    Uncached,
    /// Through sccache, with the user's cache and daemon in this directory.
    Cached(PathBuf),
}

/// How to run a compile, from the environment `var` reads.
pub fn plan(cfg: &Config, var: impl Fn(&str) -> Option<OsString>) -> Plan {
    // sccache refuses a build that turns incremental compilation on by name.
    let on = |name: &str| var(name).is_some_and(|v| v == "1");
    if on("CARGO_INCREMENTAL") || on("CARGO_BUILD_INCREMENTAL") {
        return Plan::Uncached;
    }
    // The user's own daemon, or none. ROOT/USER is root's, so the socket's
    // link in it is the module's, and it points into the daemon's runtime
    // directory, which exists only while the daemon runs and only the user
    // can reach: nobody else can stand in for the daemon. A user whose cache
    // isn't theirs (no cache, or USER naming someone else) builds without, as
    // does anyone while the daemon is down.
    let Some(user) = var("USER")
        .filter(|u| !u.is_empty())
        .or_else(|| whoami(&cfg.id))
    else {
        return Plan::Uncached;
    };
    // ROOT/USER as the words read, whatever USER holds.
    let mut base = cfg.root.clone().into_os_string();
    base.push("/");
    base.push(user);
    let base = PathBuf::from(base);
    if mine(&base.join("cache")) && socket(&base.join("sccache.sock")) {
        Plan::Cached(base)
    } else {
        Plan::Uncached
    }
}

/// `id -un`: this user's name, or none when id can't say.
fn whoami(id: &Path) -> Option<OsString> {
    let out = Command::new(id)
        .arg("-un")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    let name = String::from_utf8(out.stdout).ok()?;
    let name = name.trim_end_matches('\n');
    (!name.is_empty()).then(|| name.into())
}

/// A file there that this process's user owns.
fn mine(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.uid() == rustix::process::geteuid().as_raw())
}

fn socket(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.file_type().is_socket())
}

/// The compile through sccache, with only the module's settings: the user's
/// own SCCACHE_ variables could turn client-side mode off (SCCACHE_LOG does)
/// and send the compile to the daemon, which refuses it, and the build would
/// go uncached.
pub fn cached(cfg: &Config, base: &Path, compile: &[OsString]) -> Command {
    let mut run = Command::new(&cfg.sccache);
    run.args(compile);
    unset_sccache(&mut run);
    run.env("SCCACHE_CONF", &cfg.conf)
        .env("SCCACHE_SERVER_UDS", base.join("sccache.sock"))
        .env("SCCACHE_CLIENT_SIDE", "1");
    run
}

/// Takes every SCCACHE_ variable out of what `run` gets.
pub fn unset_sccache(run: &mut Command) {
    for (name, _) in std::env::vars_os() {
        if name.as_encoded_bytes().starts_with(b"SCCACHE_") {
            run.env_remove(name);
        }
    }
}

/// What a compile through sccache comes to.
#[derive(Debug, PartialEq, Eq)]
pub enum After {
    /// The compile's own exit, which the wrapper exits with.
    Exit(u8),
    /// Compile it again without the cache.
    Uncached,
}

/// sccache's own failures exit 2 (a socket with no daemon behind it, a
/// compiler it can't drive), as does a compile it saw killed by a signal;
/// rustc's never do, and a C compile that does only runs again. Those, and
/// an sccache that couldn't be started, compile without it. Anything else is
/// the compile's: its code, or for a signal, 128 and its number, as a shell
/// gives it.
pub fn after(ran: std::io::Result<ExitStatus>) -> After {
    let Ok(status) = ran else {
        return After::Uncached;
    };
    match (status.code(), status.signal()) {
        (Some(2), _) => After::Uncached,
        (Some(code), _) => After::Exit(u8::try_from(code).unwrap_or(1)),
        (None, Some(signal)) => After::Exit(u8::try_from(128 + signal).unwrap_or(255)),
        (None, None) => After::Exit(1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(raw: i32) -> std::io::Result<ExitStatus> {
        Ok(ExitStatus::from_raw(raw))
    }

    #[test]
    fn a_2_compiles_again_and_the_rest_are_the_compiles() {
        assert_eq!(after(status(0)), After::Exit(0));
        assert_eq!(after(status(1 << 8)), After::Exit(1));
        assert_eq!(after(status(2 << 8)), After::Uncached);
        assert_eq!(after(status(101 << 8)), After::Exit(101));
        // Killed by SIGKILL, as a shell says it.
        assert_eq!(after(status(9)), After::Exit(137));
        assert_eq!(
            after(Err(std::io::Error::from(std::io::ErrorKind::NotFound))),
            After::Uncached
        );
    }
}
