//! burrow-health CONFIG [snapshot FILE | compare FILE [SECONDS]]: HOST-health,
//! with modules/updates.nix's config (lib.rs says what each does).
//!
//! A compare's stdout is its verdict, `healthy` or what regressed, which
//! HOST-update puts in what it says; a snapshot it can't read is said there
//! too, and fails it.

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use burrow_health::{Config, compare, parse, render, report};

enum Ask {
    Now,
    Snapshot(PathBuf),
    Compare(PathBuf, u64),
}

/// The default time a compare has for everything to pass again.
const WITHIN: u64 = 180;

fn ask(words: &[OsString]) -> Option<Ask> {
    let file = |f: &OsString| (!f.is_empty()).then(|| PathBuf::from(f));
    match words {
        [] => Some(Ask::Now),
        [w, f] if w == "snapshot" => file(f).map(Ask::Snapshot),
        [w, f] if w == "compare" => file(f).map(|f| Ask::Compare(f, WITHIN)),
        [w, f, secs] if w == "compare" => {
            let secs = secs
                .to_str()
                .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))?;
            Some(Ask::Compare(file(f)?, secs.parse().ok()?))
        }
        _ => None,
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        say("usage: burrow-health CONFIG [snapshot FILE | compare FILE [SECONDS]]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            say(&format!("burrow-health: {e}"));
            return ExitCode::from(2);
        }
    };
    let Some(ask) = ask(words) else {
        say(&format!(
            "usage: {} [snapshot FILE | compare FILE [SECONDS]]",
            cfg.name
        ));
        return ExitCode::from(2);
    };
    match ask {
        Ask::Now => print(&cfg, &render(&report(&cfg)), ExitCode::SUCCESS),
        Ask::Snapshot(file) => match fs::write(&file, render(&report(&cfg))) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                say(&format!("{}: {}: {e}", cfg.name, file.display()));
                ExitCode::FAILURE
            }
        },
        Ask::Compare(file, secs) => {
            let before = match fs::read(&file) {
                Ok(text) => parse(&String::from_utf8_lossy(&text)),
                Err(e) => {
                    let why = format!(
                        "the snapshot {} can't be read ({e}), so nothing can be compared with it\n",
                        file.display()
                    );
                    return print(&cfg, &why, ExitCode::FAILURE);
                }
            };
            let within = Duration::from_secs(secs);
            match compare(&before, within, Duration::from_secs(cfg.settle), || {
                report(&cfg)
            }) {
                Ok(()) => print(&cfg, "healthy\n", ExitCode::SUCCESS),
                Err(bad) => print(&cfg, &(bad.join("\n") + "\n"), ExitCode::FAILURE),
            }
        }
    }
}

/// `text` on stdout, and `then`, unless it can't be written: then that's
/// said, and it fails.
fn print(cfg: &Config, text: &str, then: ExitCode) -> ExitCode {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => then,
        Err(e) => {
            say(&format!("{}: stdout: {e}", cfg.name));
            ExitCode::FAILURE
        }
    }
}

fn say(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}
