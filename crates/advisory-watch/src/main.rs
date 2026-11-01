//! `advisory-watch CONFIG [report]` (lib.rs). A pass needs root, for its
//! state and to run the scans and the jobs as the user, so one started as
//! anyone else starts again through sudo; a report only reads.

use std::env;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use advisory_watch::{Config, pass, state};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some((config, words)) = args.split_first() else {
        eprintln!("usage: advisory-watch CONFIG [report]");
        return ExitCode::from(2);
    };
    let cfg = match Config::read(Path::new(config)) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("advisory-watch: {e}");
            return ExitCode::from(2);
        }
    };
    match words {
        [] => one_pass(&cfg, config),
        [w] if w == "report" => report(&cfg),
        _ => {
            eprintln!("usage: advisory-watch [report]");
            ExitCode::from(2)
        }
    }
}

fn one_pass(cfg: &Config, config: &str) -> ExitCode {
    // sudo sets SUDO_UID, and makes root: a pass sudo started as someone
    // else (sudo -u) runs as them, with what they may do, rather than
    // starting again for ever.
    if !rustix::process::geteuid().is_root() && env::var_os("SUDO_UID").is_none() {
        let exe = match env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                eprintln!("advisory-watch: its own path: {e}");
                return ExitCode::FAILURE;
            }
        };
        let e = Command::new(&cfg.tools.sudo)
            .arg("--")
            .arg(exe)
            .arg(config)
            .exec();
        eprintln!("advisory-watch: {}: {e}", cfg.tools.sudo.display());
        return ExitCode::FAILURE;
    }
    let held = match state::lock(&cfg.state) {
        Ok(Some(held)) => held,
        Ok(None) => {
            println!("advisory-watch: another pass is running, so this one does nothing");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("advisory-watch: {e}");
            return ExitCode::FAILURE;
        }
    };
    let done = pass::run(cfg);
    drop(held);
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("advisory-watch: {e}");
            ExitCode::FAILURE
        }
    }
}

fn report(cfg: &Config) -> ExitCode {
    let text = match state::report(&cfg.state) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("advisory-watch: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut out = std::io::stdout().lock();
    if let Err(e) = out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        eprintln!("advisory-watch: stdout: {e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
