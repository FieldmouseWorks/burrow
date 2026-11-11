//! `burrow-dashboard CONFIG serve`, the service, and `burrow-dashboard
//! CONFIG enroll`, which prints a one-time code for adding a device. The
//! module builds CONFIG in, so on the machine they're `burrow-dashboard
//! serve` and `burrow-dashboard enroll`.

use std::env;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr};
use std::os::unix::fs::{MetadataExt, chown};
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use burrow_dashboard::enroll::{self, CODE_TTL};
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
        "enroll" => code(&cfg),
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

/// Print a fresh code. The state directory is the service's alone, so this
/// runs as root (or the service's user), and the code's file is handed to
/// whoever owns the directory.
fn code(cfg: &Config) -> Result<(), String> {
    let dir = fs::metadata(&cfg.state).map_err(|e| format!("{}: {e}", cfg.state.display()))?;
    let path = cfg.state.join("enroll.json");
    let code = enroll::issue(&path, now()).map_err(|e| match e.kind() {
        std::io::ErrorKind::PermissionDenied => format!(
            "{} is the dashboard's own: run this as root (sudo burrow-dashboard enroll)",
            cfg.state.display()
        ),
        _ => format!("{}: {e}", path.display()),
    })?;
    chown(&path, Some(dir.uid()), Some(dir.gid()))
        .map_err(|e| format!("{}: {e}", path.display()))?;
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
