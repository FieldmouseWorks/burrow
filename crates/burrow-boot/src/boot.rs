//! HOST-boot's words, on what it keeps in its state dir:
//!
//! - boot-proven: the boot bits (`bits`) of every system that came up here,
//!   a line each
//! - boot-failed: those of every trial that didn't, never tried again
//! - boot-default: the system it last made GRUB's default
//! - boot-trial: the system queued for a trial boot
//! - boot-trying: the system a trial reboot went into, until the boot after
//!   settles it (`check`)
//!
//! What it says goes to stdout, and to stderr when it refused something;
//! what the owner should hear about also goes to the journal and HOST-notify.

use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::{Broke, Config, menu};

const PROVEN: &str = "boot-proven";
const FAILED: &str = "boot-failed";
const DEFAULT: &str = "boot-default";
const TRIAL: &str = "boot-trial";
const TRYING: &str = "boot-trying";

/// How long jobs and sandboxes running can put a queued trial off, in
/// seconds: two days.
const POSTPONE: u64 = 2 * 24 * 60 * 60;

/// The notice a trial that failed opens, and one that worked closes.
const NOTICE: &str = "boot-trial";

#[derive(Debug, PartialEq, Eq)]
pub enum Word {
    /// Make SYSTEM the boot default if its boot bits came up here before;
    /// otherwise keep the default and queue SYSTEM for a trial
    /// (HOST-confirm says this).
    Default(PathBuf),
    /// Reboot into the queued trial once, if the box is idle (`force`:
    /// whatever jobs and sandboxes are running).
    Trial {
        force: bool,
    },
    /// After a boot: settle a trial, and remember what came up.
    Check,
    Status,
}

/// Runs `word`: what it exits with, or why it broke.
pub fn run(cfg: &Config, word: &Word) -> Result<u8, Broke> {
    let boot = Boot::open(cfg)?;
    match word {
        Word::Default(system) => boot.default(system),
        Word::Trial { force } => boot.trial(*force),
        Word::Check => boot.check(),
        Word::Status => boot.status(),
    }
}

struct Boot<'a> {
    cfg: &'a Config,
}

impl<'a> Boot<'a> {
    /// Its state dir, made if it's new, with both lists in it.
    fn open(cfg: &'a Config) -> Result<Boot<'a>, Broke> {
        fs::create_dir_all(&cfg.state).map_err(|e| at(&cfg.state, e))?;
        let boot = Boot { cfg };
        for list in [PROVEN, FAILED] {
            let path = boot.file(list);
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| at(&path, e))?;
        }
        Ok(boot)
    }

    fn file(&self, name: &str) -> PathBuf {
        self.cfg.state.join(name)
    }

    fn default(&self, system: &Path) -> Result<u8, Broke> {
        let new = canonical(system)?;
        let bits = bits(&new)?;
        if self.proven(&bits)? {
            self.make_default(&new)?;
            self.unqueue()?;
            say(&format!("boot default: {}", new.display()));
        } else if self.listed(FAILED, &bits)? {
            // Its kernel failed to boot here once: never queued again.
            let d = self.current_default()?;
            self.make_default(&d)?;
            complain(&format!(
                "boot default stays {}: {}'s kernel failed to boot before",
                d.display(),
                new.display()
            ));
        } else {
            // The menu made again, so the new system is in it, with the
            // proven default still on top; and the new one queued.
            let d = self.current_default()?;
            self.make_default(&d)?;
            self.write(TRIAL, &new)?;
            say(&format!(
                "boot default stays {}; {} boots once in the next trial ({} trial)",
                d.display(),
                new.display(),
                self.cfg.boot
            ));
        }
        Ok(0)
    }

    fn trial(&self, force: bool) -> Result<u8, Broke> {
        let Some(new) = self.read(TRIAL)? else {
            say("no trial queued");
            return Ok(0);
        };
        if self.proven(&bits(&new)?)? {
            self.make_default(&new)?;
            self.unqueue()?;
            say(&format!(
                "already proven; {} is the boot default",
                new.display()
            ));
            return Ok(0);
        }
        if !force && self.queued_for()? < POSTPONE && self.busy() {
            say("jobs or sandboxes running; trial postponed");
            return Ok(0);
        }
        // Never with an unconfirmed switch live: a reboot would decide it.
        if self.cfg.switch_state.join("pending").exists() {
            say("a switch is pending; trial postponed");
            return Ok(0);
        }
        let grub_cfg = &self.cfg.grub_cfg;
        let menu = fs::read(grub_cfg).map_err(|e| at(grub_cfg, e))?;
        let Some(title) = menu::title_of(&menu, &new) else {
            return Err(Broke(format!("{} is not in the GRUB menu", new.display())));
        };
        self.write(TRYING, &new)?;
        let mut next = OsString::from("next_entry=");
        next.push(&title);
        if let Err(Broke(why)) = self.editenv(&["set".as_ref(), &next]) {
            // Taken back from every grubenv, so the boot after this isn't a
            // trial. While one may still hold it, boot-trying stays: the
            // boot after is checked as the trial it may be.
            let taken_back = self
                .editenv(&["unset".as_ref(), "next_entry".as_ref()])
                .and_then(|()| remove(&self.file(TRYING)));
            return Err(match taken_back {
                Ok(()) => Broke(why),
                Err(Broke(also)) => Broke(format!("{why}; taking it back: {also}")),
            });
        }
        rustix::fs::sync();
        self.journal(
            "warning",
            &format!("rebooting once into: {}", title.to_string_lossy()),
        );
        self.reboot()?;
        Ok(0)
    }

    /// Settles a trial, which either came up (booted, with sshd running)
    /// and is the boot default from now on, or didn't and is never tried
    /// again; and remembers that what booted came up. A step of a failed
    /// trial that breaks doesn't stop the rest, rebooting out of it most of
    /// all, and nor does a notice that won't open or close: each is said,
    /// and check exits 1.
    fn check(&self) -> Result<u8, Broke> {
        let cfg = self.cfg;
        let booted = canonical(&cfg.booted)?;
        let trying = self.read(TRYING)?;
        remove(&self.file(TRYING))?;
        let mut ok = true;
        // GRUB clears next_entry itself; make sure, so a missed clear never
        // loops.
        noted(
            &mut ok,
            &cfg.boot,
            self.editenv(&["unset".as_ref(), "next_entry".as_ref()]),
        );
        if let Some(trying) = &trying {
            let came_up = booted == *trying;
            if !came_up || !self.sshd_active() {
                noted(
                    &mut ok,
                    &cfg.boot,
                    bits(trying).and_then(|b| self.add(FAILED, &b)),
                );
                noted(&mut ok, &cfg.boot, self.unqueue());
                let (t, b) = (trying.display(), booted.display());
                if !came_up {
                    self.journal(
                        "err",
                        &format!("TRIAL FAILED: {t} did not come up; booted {b} instead"),
                    );
                    let notified = self.notify(&[
                        "open",
                        NOTICE,
                        "A new kernel didn't boot",
                        &format!(
                            "{} tried to boot {t} once and came back up on {b} instead (panic, hang or watchdog reset). That kernel won't be tried again. The default is still the last good one. Claude is looking into it.",
                            cfg.host
                        ),
                    ]);
                    noted(&mut ok, &cfg.boot, notified);
                    return Ok(if ok { 0 } else { 1 });
                }
                // Through multi-user, but broken: back to the proven default
                // (next_entry is cleared already).
                self.journal(
                    "err",
                    &format!("TRIAL FAILED: {t} booted without sshd; rebooting into the default"),
                );
                let notified = self.notify(&[
                    "open",
                    NOTICE,
                    "A new kernel booted broken",
                    &format!(
                        "{} booted {t} once, but sshd didn't come up, so it rebooted into the last good system. That kernel won't be tried again.",
                        cfg.host
                    ),
                ]);
                noted(&mut ok, &cfg.boot, notified);
                self.reboot()?;
                return Ok(if ok { 0 } else { 1 });
            }
        }
        // This kernel, initrd and command line work here.
        let bits = bits(&booted)?;
        if !self.listed(PROVEN, &bits)? {
            self.add(PROVEN, &bits)?;
        }
        if trying.is_some() {
            self.make_default(&booted)?;
            self.unqueue()?;
            self.journal(
                "notice",
                &format!("trial passed: {} is the boot default", booted.display()),
            );
            let notified = self.notify(&[
                "close",
                NOTICE,
                &format!("The trial boot of {} worked.", booted.display()),
            ]);
            noted(&mut ok, &cfg.boot, notified);
        }
        Ok(if ok { 0 } else { 1 })
    }

    fn status(&self) -> Result<u8, Broke> {
        let cfg = self.cfg;
        let trial = match self.read(TRIAL)? {
            Some(t) => t.display().to_string(),
            None => "none".into(),
        };
        say(&format!(
            "booted:       {}\nrunning:      {}\nboot default: {}\ntrial queued: {trial}",
            canonical(&cfg.booted)?.display(),
            canonical(&cfg.current)?.display(),
            self.current_default()?.display(),
        ));
        Ok(0)
    }

    /// Whether `bits` came up here: they're what's running since boot, or
    /// on the proven list.
    fn proven(&self, bits: &str) -> Result<bool, Broke> {
        Ok(bits == self::bits(&canonical(&self.cfg.booted)?)? || self.listed(PROVEN, bits)?)
    }

    /// Whether `bits` is a line of `list`.
    fn listed(&self, list: &str, bits: &str) -> Result<bool, Broke> {
        let path = self.file(list);
        let text = fs::read(&path).map_err(|e| at(&path, e))?;
        Ok(text.split(|&b| b == b'\n').any(|l| l == bits.as_bytes()))
    }

    fn add(&self, list: &str, bits: &str) -> Result<(), Broke> {
        let path = self.file(list);
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| writeln!(f, "{bits}"))
            .map_err(|e| at(&path, e))
    }

    /// The system GRUB boots by default now: the last one this made the
    /// default while it's still a system that came up here, else what's
    /// running since boot.
    fn current_default(&self) -> Result<PathBuf, Broke> {
        // As good as none when it can't be read.
        if let Ok(Some(d)) = self.read(DEFAULT)
            && d.join("bin/switch-to-configuration").exists()
            && let Ok(bits) = bits(&d)
            && self.proven(&bits)?
        {
            return Ok(d);
        }
        canonical(&self.cfg.booted)
    }

    fn make_default(&self, system: &Path) -> Result<(), Broke> {
        ran(&system.join("bin/switch-to-configuration"), ["boot"])?;
        self.write(DEFAULT, system)
    }

    /// How long ago, in seconds, the trial was queued.
    fn queued_for(&self) -> Result<u64, Broke> {
        let path = self.file(TRIAL);
        let queued = fs::metadata(&path)
            .and_then(|m| m.modified())
            .map_err(|e| at(&path, e))?;
        Ok(SystemTime::now()
            .duration_since(queued)
            .map_or(0, |d| d.as_secs()))
    }

    /// Agent work in flight that a reboot would cut off: the user's Claude
    /// jobs, and sandboxes.
    fn busy(&self) -> bool {
        let systemctl = &self.cfg.tools.systemctl;
        let units = |args: &[&str]| {
            Command::new(systemctl)
                .args(args)
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
                .map_or(0, |o| o.stdout.iter().filter(|&&b| b == b'\n').count())
        };
        let list = ["list-units", "--plain", "--no-legend", "--state=active"];
        let user = format!("{}@", self.cfg.user);
        let jobs = units(&[&["--user", "-M", &user][..], &list, &["proj-*.service"]].concat());
        let sandboxes = units(&[&list[..], &["microvm@*.service"]].concat());
        jobs + sandboxes > 0
    }

    fn sshd_active(&self) -> bool {
        Command::new(&self.cfg.tools.systemctl)
            .args(["-q", "is-active", "sshd.service"])
            .stdin(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// `args` in each grubenv beside a GRUB (in /boot, and a mirrored
    /// /boot-fallback), made first if it's missing. Each is tried, even
    /// after one fails: what failed, said together.
    fn editenv(&self, args: &[&OsStr]) -> Result<(), Broke> {
        let editenv = &self.cfg.tools.grub_editenv;
        let mut failed = Vec::new();
        for dir in &self.cfg.grub_dirs {
            let grub = dir.join("grub");
            if !grub.exists() {
                continue;
            }
            let env = grub.join("grubenv");
            let made = if env.exists() {
                Ok(())
            } else {
                ran(editenv, [env.as_os_str(), "create".as_ref()])
            };
            let edited = made.and_then(|()| ran(editenv, [&[env.as_os_str()][..], args].concat()));
            if let Err(Broke(why)) = edited {
                failed.push(why);
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(Broke(failed.join("; ")))
        }
    }

    fn reboot(&self) -> Result<(), Broke> {
        ran(&self.cfg.tools.systemctl, ["reboot"])
    }

    /// `line`, in the journal at `priority`, tagged as HOST-boot; on stderr
    /// when the journal won't take it.
    fn journal(&self, priority: &str, line: &str) {
        let cfg = self.cfg;
        let logged = Command::new(&cfg.tools.logger)
            .args(["-t", &cfg.boot, "-p", priority])
            .stdin(Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                if let Some(mut stdin) = c.stdin.take() {
                    writeln!(stdin, "{line}")?;
                }
                c.wait()
            });
        if !logged.is_ok_and(|s| s.success()) {
            complain(line);
        }
    }

    /// HOST-notify, told `words`: whether the notice opened or closed.
    fn notify(&self, words: &[&str]) -> Result<(), Broke> {
        let notify = &self.cfg.tools.notify;
        match Command::new(notify)
            .args(words)
            .stdin(Stdio::null())
            .status()
        {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(Broke(format!("{} {}: {s}", notify.display(), words[0]))),
            Err(e) => Err(Broke(format!("{}: {e}", notify.display()))),
        }
    }

    /// The system in the state file `name`, if there is one.
    fn read(&self, name: &str) -> Result<Option<PathBuf>, Broke> {
        let path = self.file(name);
        match fs::read(&path) {
            Ok(mut text) => {
                while text.last() == Some(&b'\n') {
                    text.pop();
                }
                Ok((!text.is_empty()).then(|| OsString::from_vec(text).into()))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(at(&path, e)),
        }
    }

    fn write(&self, name: &str, system: &Path) -> Result<(), Broke> {
        let path = self.file(name);
        fs::write(&path, [system.as_os_str().as_bytes(), b"\n"].concat()).map_err(|e| at(&path, e))
    }

    /// No trial queued.
    fn unqueue(&self) -> Result<(), Broke> {
        remove(&self.file(TRIAL))
    }
}

/// What the bootloader actually hands over to a system: its kernel, initrd
/// and command line (init= aside, which is the system itself and changes
/// with every generation), hashed. Every list a machine kept before this
/// was Rust has them the same: the SHA-256, in hex, of the kernel's and the
/// initrd's paths and the params without their trailing newlines, joined
/// with spaces.
pub fn bits(system: &Path) -> Result<String, Broke> {
    let kernel = canonical(&system.join("kernel"))?;
    let initrd = canonical(&system.join("initrd"))?;
    let path = system.join("kernel-params");
    let mut params = fs::read(&path).map_err(|e| at(&path, e))?;
    while params.last() == Some(&b'\n') {
        params.pop();
    }
    let text = [
        kernel.as_os_str().as_bytes(),
        b" ",
        initrd.as_os_str().as_bytes(),
        b" ",
        &params,
    ]
    .concat();
    Ok(Sha256::digest(&text)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// `path` with every symlink in it followed.
fn canonical(path: &Path) -> Result<PathBuf, Broke> {
    fs::canonicalize(path).map_err(|e| at(path, e))
}

fn remove(path: &Path) -> Result<(), Broke> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(at(path, e)),
        _ => Ok(()),
    }
}

/// `program` run with `args`, its output its own: whether it worked.
fn ran<A: AsRef<OsStr>>(program: &Path, args: impl IntoIterator<Item = A>) -> Result<(), Broke> {
    let args: Vec<A> = args.into_iter().collect();
    let shown = || {
        let words: Vec<_> = args.iter().map(|a| a.as_ref().to_string_lossy()).collect();
        format!("{} {}", program.display(), words.join(" "))
    };
    match Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(Broke(format!("{}: {s}", shown()))),
        Err(e) => Err(Broke(format!("{}: {e}", shown()))),
    }
}

fn at(path: &Path, e: io::Error) -> Broke {
    Broke(format!("{}: {e}", path.display()))
}

/// A step that broke, said, and `ok` no longer.
fn noted(ok: &mut bool, boot: &str, step: Result<(), Broke>) {
    if let Err(Broke(why)) = step {
        complain(&format!("{boot}: {why}"));
        *ok = false;
    }
}

/// What it says: on stdout, whether or not anything reads it.
fn say(line: &str) {
    let _ = writeln!(io::stdout().lock(), "{line}");
}

fn complain(line: &str) {
    let _ = writeln!(io::stderr().lock(), "{line}");
}
