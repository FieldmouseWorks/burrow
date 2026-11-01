//! HOST-backup: the machine's data, off the machine. Each new snapshot of
//! the datasets whose name matches burrow.backup.snapshots goes to a restic
//! repository, read from the snapshot itself at the datasets' own paths, so
//! the copy is one moment of the box and the next run reads and sends only
//! what changed.
//!
//!   HOST-backup            back up the newest snapshot, if it isn't yet; once
//!                          a day, also forget old backups, prune, read a share
//!                          of the repository back and run the drill (pass.rs,
//!                          upkeep.rs)
//!   HOST-backup status     the last backup, and what's in the repository
//!   HOST-backup drill [PATH...]
//!                          check the newest backup holds everything its
//!                          snapshot had, then restore PATHs (default
//!                          burrow.backup.drill) to scratch and compare them
//!                          with it, byte for byte (drill.rs, compare.rs)
//!   HOST-backup restic ARGS...
//!                          restic itself, on this repository (to restore by
//!                          hand: restic restore latest --target DIR --include PATH)
//!
//! Restic encrypts everything before it leaves, with the password in the
//! password file: without it the repository is noise, so its owner keeps a
//! copy off the box. modules/backup.nix builds the config in.

pub mod compare;
pub mod config;
pub mod drill;
pub mod glob;
pub mod listing;
pub mod mounts;
pub mod pass;
pub mod snapshot;
pub mod status;
pub mod upkeep;
pub mod zfs;

pub use config::Config;

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{Duration, SystemTime};

/// One run of HOST-backup: its config, and how to start itself again.
pub struct Run<'a> {
    pub cfg: &'a Config,
    /// This program and its config, to run again in a snapshot's namespace.
    pub exe: PathBuf,
    pub config: PathBuf,
    path: OsString,
}

impl<'a> Run<'a> {
    pub fn new(cfg: &'a Config, config: &Path) -> Result<Run<'a>, String> {
        let exe = std::env::current_exe().map_err(|e| format!("its own path: {e}"))?;
        let mut path = OsString::from(&cfg.path);
        if let Some(given) = std::env::var_os("PATH").filter(|p| !p.is_empty()) {
            path.push(":");
            path.push(given);
        }
        Ok(Run {
            cfg,
            exe,
            config: config.to_owned(),
            path,
        })
    }

    /// A program, found on its PATH.
    pub fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut c = Command::new(program);
        c.env("PATH", &self.path);
        c
    }

    /// restic, on the repository, as the live box has it.
    pub fn restic(&self) -> Command {
        let mut c = self.command("restic");
        c.envs(restic_env(
            self.cfg,
            &self.cfg.state,
            &self.cfg.password_file,
        ));
        c
    }

    /// This program again, with its config.
    pub fn itself(&self) -> Vec<OsString> {
        vec![self.exe.clone().into(), self.config.clone().into()]
    }
}

/// The environment restic runs with, its state in `state` and its password
/// in `password`.
pub fn restic_env(cfg: &Config, state: &Path, password: &Path) -> Vec<(&'static str, OsString)> {
    let mut env = vec![
        ("RESTIC_REPOSITORY", OsString::from(&cfg.repository)),
        ("RESTIC_PASSWORD_FILE", password.into()),
        ("RESTIC_CACHE_DIR", state.join("cache").into()),
        ("RESTIC_PACK_SIZE", cfg.pack_size.to_string().into()),
        ("RESTIC_COMPRESSION", "auto".into()),
        // Each mount of a snapshot is a device of its own: without this,
        // restic would write every directory's tree again on every run.
        ("RESTIC_FEATURES", "device-id-for-hardlinks".into()),
    ];
    if cfg.rclone_config_file.is_some() {
        env.push(("RCLONE_CONFIG", state.join("rclone.conf").into()));
    }
    env
}

/// The rules every backup follows, the drill's included.
pub fn backup_flags(cfg: &Config) -> Vec<OsString> {
    let mut f: Vec<OsString> = [
        "--one-file-system",
        "--exclude-caches",
        "--exclude-if-present",
        ".nobackup",
        "--exclude-file",
    ]
    .map(OsString::from)
    .into();
    f.push(cfg.exclude_file.clone().into());
    f
}

/// The status as the shell gives it: a signal's is 128 and its number.
pub fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}

/// The last `n` lines of what a program said.
pub fn tail(text: &[u8], n: usize) -> String {
    let text = String::from_utf8_lossy(text);
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// A file's text without the newlines at its end, as `$(cat FILE)` has it,
/// or None when there's no file.
pub fn read(path: &Path) -> Result<Option<String>, String> {
    match fs::read(path) {
        Ok(b) => Ok(Some(
            String::from_utf8_lossy(&b)
                .trim_end_matches('\n')
                .to_owned(),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Writes `text` to `path` whole or not at all, mode 600.
pub fn write(path: &Path, text: &[u8]) -> Result<(), String> {
    let mut new = path.as_os_str().to_owned();
    new.push(".new");
    let new = PathBuf::from(new);
    let io = |e: std::io::Error| format!("{}: {e}", new.display());
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&new)
        .map_err(io)?;
    f.write_all(text).and_then(|()| f.sync_all()).map_err(io)?;
    fs::rename(&new, path).map_err(|e| format!("{}: {e}", path.display()))
}

/// Takes `path` away, if it's there.
pub fn remove(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", path.display()))
        }
        _ => Ok(()),
    }
}

/// Marks `path` as done now, as `touch` does.
pub fn touch(path: &Path) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", path.display());
    let f: File = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(io)?;
    f.set_modified(SystemTime::now()).map_err(io)
}

/// Seconds since the epoch.
pub fn now() -> i64 {
    jiff::Timestamp::now().as_second()
}

/// How long before `now` a state file was written: none yet when it was
/// written after (the clock moved back), so a job started then is still on
/// it and the day's upkeep is still done, as the shell's negative ages had
/// it.
pub fn age(written: SystemTime, now: SystemTime) -> Duration {
    now.duration_since(written).unwrap_or(Duration::ZERO)
}

/// A time, as `date -Is` writes it, in the machine's time zone.
pub fn stamp(epoch: i64) -> String {
    zoned(epoch).map_or_else(
        || epoch.to_string(),
        |z| z.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string(),
    )
}

/// A time, as `date '+%F %T'` writes it, in the machine's time zone: how
/// restic's `--time` takes it.
pub fn local(epoch: i64) -> String {
    zoned(epoch).map_or_else(|| epoch.to_string(), |z| z.strftime("%F %T").to_string())
}

fn zoned(epoch: i64) -> Option<jiff::Zoned> {
    let t = jiff::Timestamp::from_second(epoch).ok()?;
    Some(t.to_zoned(jiff::tz::TimeZone::system()))
}

/// A backup's short ID, as restic shows it.
pub fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// rclone writes refreshed tokens back to its config, and some remotes'
/// refresh tokens work only once: the backup works on a copy of its own
/// that keeps them, started over only when the secret it came from changes.
/// The secret's hash is kept as `sha256sum` printed it, so the copy carries
/// on from the shell's.
pub fn rclone(cfg: &Config) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let Some(secret) = &cfg.rclone_config_file else {
        return Ok(());
    };
    let text = fs::read(secret)
        .map_err(|e| format!("can't read rclone's config at {} ({e})", secret.display()))?;
    let hex: String = Sha256::digest(&text)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let seed = format!("{hex}  -");
    let copy = cfg.state.join("rclone.conf");
    let kept = read(&cfg.state.join("rclone.seed"))?;
    let empty = fs::metadata(&copy).map_or(true, |m| m.len() == 0);
    if empty || kept.as_deref() != Some(seed.as_str()) {
        write(&copy, &text)?;
        write(
            &cfg.state.join("rclone.seed"),
            format!("{seed}\n").as_bytes(),
        )?;
    }
    Ok(())
}

/// The state directory, there and readable by root alone.
pub fn private_state(cfg: &Config) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let state = &cfg.state;
    fs::create_dir_all(state).map_err(|e| format!("{}: {e}", state.display()))?;
    fs::set_permissions(state, fs::Permissions::from_mode(0o700)).map_err(|e| {
        format!(
            "{}: {e}, so its state can't be kept private",
            state.display()
        )
    })
}

/// One at a time: a backup, a drill, a restore by hand. Each of them may
/// refresh rclone's token.
pub fn lock_path(cfg: &Config) -> PathBuf {
    PathBuf::from(format!("/run/{}.lock", cfg.name))
}

/// The lock, and whether it was free.
pub fn lock(path: &Path) -> Result<(File, bool), String> {
    use rustix::fs::{FlockOperation, flock};
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    match flock(&file, FlockOperation::NonBlockingLockExclusive) {
        Ok(()) => Ok((file, true)),
        Err(rustix::io::Errno::WOULDBLOCK) => Ok((file, false)),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// The lock, waiting for it if someone else holds it.
pub fn hold(path: &Path) -> Result<File, String> {
    use rustix::fs::{FlockOperation, flock};
    let (file, free) = lock(path)?;
    if !free {
        eprintln!("waiting for the backup that's running");
        flock(&file, FlockOperation::LockExclusive).map_err(|e| format!("the lock: {e}"))?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_written_after_now_is_no_age() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let hour = Duration::from_secs(3600);
        assert_eq!(age(now - hour, now), hour);
        assert_eq!(age(now, now), Duration::ZERO);
        assert_eq!(age(now + hour, now), Duration::ZERO);
    }

    #[test]
    fn rclones_copy_is_kept_until_its_secret_changes() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config::for_tests(dir.path());
        let secret = dir.path().join("secret.conf");
        let (copy, seed) = (
            dir.path().join("rclone.conf"),
            dir.path().join("rclone.seed"),
        );
        assert_eq!(
            rclone(&cfg),
            Err(format!(
                "can't read rclone's config at {} (No such file or directory (os error 2))",
                secret.display()
            ))
        );
        fs::write(&secret, "abc").unwrap();
        rclone(&cfg).unwrap();
        assert_eq!(fs::read_to_string(&copy).unwrap(), "abc");
        // As `sha256sum <FILE` printed it, so a copy the shell made carries on.
        assert_eq!(
            fs::read_to_string(&seed).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  -\n"
        );
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::write(&copy, "abc\n# refreshed\n").unwrap();
        rclone(&cfg).unwrap();
        assert_eq!(fs::read_to_string(&copy).unwrap(), "abc\n# refreshed\n");
        // The shell's seed, with no newline after it, is the same seed.
        fs::write(
            &seed,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  -",
        )
        .unwrap();
        rclone(&cfg).unwrap();
        assert_eq!(fs::read_to_string(&copy).unwrap(), "abc\n# refreshed\n");
        // An empty copy starts over.
        fs::write(&copy, "").unwrap();
        rclone(&cfg).unwrap();
        assert_eq!(fs::read_to_string(&copy).unwrap(), "abc");
        fs::write(&copy, "abc\n# refreshed\n").unwrap();
        fs::write(&secret, "rotated").unwrap();
        rclone(&cfg).unwrap();
        assert_eq!(fs::read_to_string(&copy).unwrap(), "rotated");
        // With no rclone, nothing.
        let none = config::Config {
            rclone_config_file: None,
            ..config::for_tests(&dir.path().join("none"))
        };
        rclone(&none).unwrap();
        assert!(!dir.path().join("none").exists());
    }

    #[test]
    fn state_files_are_read_as_the_shell_read_them() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("f");
        assert_eq!(read(&f), Ok(None));
        fs::write(&f, "snap-1\n\n").unwrap();
        assert_eq!(read(&f), Ok(Some("snap-1".to_owned())));
        write(&f, b"snap-2\n").unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "snap-2\n");
        assert!(!dir.path().join("f.new").exists());
        remove(&f).unwrap();
        remove(&f).unwrap();
        assert_eq!(read(&f), Ok(None));
    }

    #[test]
    fn the_last_lines_are_the_tail() {
        assert_eq!(tail(b"a\nb\nc\nd\n", 3), "b\nc\nd");
        assert_eq!(tail(b"a", 3), "a");
        assert_eq!(tail(b"", 3), "");
    }

    #[test]
    fn times_are_said_as_date_says_them() {
        let s = stamp(1_700_000_000);
        assert_eq!(s.len(), "2023-11-14T22:13:20+00:00".len(), "{s}");
        assert_eq!(local(1_700_000_000).len(), "2023-11-14 22:13:20".len());
        assert_eq!(short("0123456789abcdef"), "01234567");
        assert_eq!(short("0123"), "0123");
    }
}
