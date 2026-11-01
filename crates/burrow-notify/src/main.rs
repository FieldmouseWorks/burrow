//! `burrow-notify CONFIG open|close|pending KEY ...`, which is HOST-notify
//! (lib.rs) once modules/notify.nix builds the config in. It exits 0 whatever
//! GitHub does, 2 on a usage error, and `pending` answers 0 or 1.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use burrow_github::Gh;
use burrow_notify::{Ask, Config, Journal, Priority, USAGE, pending, run};

fn main() -> ExitCode {
    let raw: Vec<OsString> = env::args_os().skip(1).collect();
    let Some(args) = raw
        .iter()
        .map(|a| a.clone().into_string().ok())
        .collect::<Option<Vec<String>>>()
    else {
        eprintln!("burrow-notify: its words must be text");
        return ExitCode::from(2);
    };
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: burrow-notify CONFIG {USAGE}");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-notify: {e}");
            return ExitCode::from(2);
        }
    };
    let ask = match Ask::parse(words) {
        Ok(ask) => ask,
        Err(e) => {
            eprintln!("{}: {e}", cfg.name);
            return ExitCode::from(2);
        }
    };
    // The notices' files are anyone's to read, so this asks nothing of the
    // user or of GitHub.
    if let Ask::Pending { key } = &ask {
        return ExitCode::from(if pending(&cfg, key) { 0 } else { 1 });
    }
    let mut journal = Systemd(cfg.name.clone());
    let who = whoami();
    if who.as_deref() != Some(cfg.user.as_str()) {
        // Once is enough: a runuser that didn't make it the user would run
        // it again and again.
        if env::var_os(AS_USER).is_some() {
            journal.log(
                Priority::Warning,
                &format!(
                    "{} {}: runuser ran it as {}, not {}",
                    cfg.name,
                    words[0],
                    who.as_deref().unwrap_or("nobody id knows"),
                    cfg.user
                ),
            );
            return ExitCode::SUCCESS;
        }
        // gh needs the user's login, or the App's socket, which the user
        // may reach.
        let e = as_user(&cfg, &raw);
        journal.log(
            Priority::Warning,
            &format!(
                "{} {}: couldn't run as {}: {e}",
                cfg.name, words[0], cfg.user
            ),
        );
        return ExitCode::SUCCESS;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    run(&cfg, &ask, &mut Gh::default(), &mut journal, now);
    ExitCode::SUCCESS
}

/// Set in what it runs as the user.
const AS_USER: &str = "BURROW_NOTIFY_AS_USER";

fn whoami() -> Option<String> {
    let out = Command::new("id")
        .arg("-un")
        .stderr(Stdio::inherit())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Runs this again, the same words and all, as the user. Returns only if it
/// can't.
fn as_user(cfg: &Config, args: &[OsString]) -> String {
    // Theirs to write: made here, since only root may make it.
    let _ = fs::create_dir_all(&cfg.dir);
    let _ = Command::new("chown")
        .arg(format!("{}:", cfg.user))
        .arg(&cfg.dir)
        .stderr(Stdio::null())
        .status();
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return format!("its own path: {e}"),
    };
    let mut home = OsString::from("HOME=");
    home.push(&cfg.home);
    let e = Command::new("runuser")
        .args(["-u", &cfg.user, "--", "env"])
        .arg(home)
        .arg(format!("{AS_USER}=1"))
        .arg(exe)
        .args(args)
        .exec();
    format!("runuser: {e}")
}

/// The journal, through systemd-cat, under the command's own name.
struct Systemd(String);

impl Journal for Systemd {
    fn log(&mut self, priority: Priority, line: &str) {
        let level = match priority {
            Priority::Warning => "warning",
            Priority::Notice => "notice",
        };
        let sent = Command::new("systemd-cat")
            .args(["-t", &self.0, "-p", level])
            .stdin(Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                let mut stdin = child.stdin.take().expect("piped");
                stdin.write_all(format!("{line}\n").as_bytes())?;
                drop(stdin);
                child.wait()
            });
        if let Err(e) = sent {
            eprintln!("{}: systemd-cat: {e}: {line}", self.0);
        }
    }
}
