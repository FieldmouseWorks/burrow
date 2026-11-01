//! `burrow-browser-close PORT`: the browser unit's ExecStop=, which systemd
//! runs with the unit's main process in MAINPID.

use std::env;
use std::process::ExitCode;

use burrow_browser_close::{EXIT_WITHIN, close};

const USAGE: &str =
    "usage: burrow-browser-close PORT (the browser unit's ExecStop=, with MAINPID set)";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(port) = (match args.as_slice() {
        [port] => port.parse::<u16>().ok(),
        _ => None,
    }) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let Some(pid) = env::var("MAINPID").ok().and_then(|p| p.parse::<u32>().ok()) else {
        eprintln!("burrow-browser-close: MAINPID isn't a process ID ({USAGE})");
        return ExitCode::from(2);
    };
    match close(port, pid, EXIT_WITHIN) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-browser-close: {e}");
            ExitCode::FAILURE
        }
    }
}
