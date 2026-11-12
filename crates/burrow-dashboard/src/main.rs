//! `burrow-dashboard CONFIG serve`, the service, and `burrow-dashboard
//! CONFIG enroll`, which prints a one-time code for adding a device. The
//! module builds CONFIG in, so on the machine they're `burrow-dashboard
//! serve` and `burrow-dashboard enroll`.

use std::env;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::MetadataExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};
use std::sync::Arc;

use burrow_dashboard::enroll::{CODE_TTL, Codes};
use burrow_dashboard::pages::span;
use burrow_dashboard::web::{App, router};
use burrow_dashboard::{Config, now};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let (config, command) = match args.as_slice() {
        [config, command] => (Path::new(config), command.as_str()),
        _ => return usage(),
    };
    let cfg = match Config::read(config) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("burrow-dashboard: {e}");
            return ExitCode::from(2);
        }
    };
    let done = match command {
        "serve" => serve(cfg),
        "enroll" => code(&cfg, config),
        _ => return usage(),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-dashboard: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: burrow-dashboard serve   run the dashboard\n       \
         burrow-dashboard enroll  print a one-time code that adds a device's passkey"
    );
    ExitCode::from(2)
}

/// Print a fresh code. The state directory is the service's alone. Run as
/// root, this runs itself again as the directory's owner before it touches
/// anything in it, so root never opens a path the service's user could
/// have pointed elsewhere.
fn code(cfg: &Config, config: &Path) -> Result<(), String> {
    let dir = fs::metadata(&cfg.state).map_err(|e| format!("{}: {e}", cfg.state.display()))?;
    // The process's own directory is its effective user's.
    let me = fs::metadata("/proc/self")
        .map_err(|e| format!("/proc/self: {e}"))?
        .uid();
    if me != dir.uid() {
        if me != 0 {
            return Err(format!(
                "{} is the dashboard's own: run this as root (sudo burrow-dashboard enroll)",
                cfg.state.display()
            ));
        }
        let exe = env::current_exe().map_err(|e| format!("this program: {e}"))?;
        // std drops root's supplementary groups along with its uid.
        let status = Command::new(exe)
            .arg(config)
            .arg("enroll")
            .uid(dir.uid())
            .gid(dir.gid())
            .env_clear()
            .current_dir("/")
            .status()
            .map_err(|e| format!("as the dashboard's user: {e}"))?;
        return match status.success() {
            true => Ok(()),
            false => Err(format!("as the dashboard's user, it failed ({status})")),
        };
    }
    let code = Codes::new(&cfg.state, cfg.owner.id)
        .issue(now())
        .map_err(|e| format!("{}: {e}", cfg.state.display()))?;
    println!("{code}");
    eprintln!(
        "Type it on the device at {}/enroll within {}. It adds one passkey; a new code replaces it.",
        cfg.origin,
        span(CODE_TTL)
    );
    Ok(())
}

fn serve(cfg: Config) -> Result<(), String> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, cfg.port));
    let app = Arc::new(App::new(cfg)?);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async move {
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| format!("{addr}: {e}"))?;
        eprintln!("burrow-dashboard: serving {} on {addr}", app.cfg.origin);
        // Stopped, it just goes: every file is written whole as it changes.
        axum::serve(listener, router(app))
            .await
            .map_err(|e| e.to_string())
    })
}
