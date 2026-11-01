//! house-sync CONFIG [status] [OWNER/REPO]: see lib.rs.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use burrow_github::Gh;
use house_sync::config::Config;
use house_sync::pass::{self, Ask, Notify};

struct Notifier(PathBuf);

impl Notify for Notifier {
    fn notify(&mut self, args: &[&str]) -> Result<(), String> {
        let ran = Command::new(&self.0).args(args).status();
        match ran {
            Ok(s) if s.success() => Ok(()),
            Ok(s) => Err(format!("{} {}: {s}", self.0.display(), args[..2].join(" "))),
            Err(e) => Err(format!("{}: {e}", self.0.display())),
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let (config, dry, only) = match words.as_slice() {
        [config] => (config, false, None),
        [config, "status"] => (config, true, None),
        [config, "status", only] => (config, true, Some(*only)),
        [config, only] if !only.starts_with('-') => (config, false, Some(*only)),
        _ => {
            eprintln!("usage: house-sync CONFIG [status] [OWNER/REPO]");
            return ExitCode::from(2);
        }
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("house-sync: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut notify = Notifier(cfg.notify.clone());
    let ran = pass::run(
        &cfg,
        &Ask { dry, only },
        &mut Gh,
        &mut notify,
        &mut std::io::stdout(),
    );
    match ran {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(e) => {
            eprintln!("house-sync: {e}");
            ExitCode::FAILURE
        }
    }
}
