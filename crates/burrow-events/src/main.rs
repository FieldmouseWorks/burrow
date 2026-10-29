//! burrow-events, as modules/events.nix runs it: EVENTS_CONFIG names its
//! config, and the webhook's secret is the `webhook` credential.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::{env, fs, thread};

use burrow_events::handler::Config;
use burrow_events::line::{DEBOUNCE, Line, Pass};
use burrow_events::passes::{Systemctl, serve};
use burrow_events::routes::{Handlers, Lines, delivery, kick};
use burrow_events::{http, now};

fn main() -> ExitCode {
    match run() {
        // The servers and the handlers' lines run until the service stops,
        // with no handlers too: exiting would be a restart every few seconds.
        Ok(()) => loop {
            thread::park();
        },
        Err(e) => {
            eprintln!("burrow-events: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let path = env::var_os("EVENTS_CONFIG").ok_or("EVENTS_CONFIG isn't set")?;
    let text = fs::read(&path).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;
    let cfg: Config =
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;
    let handlers: Handlers = Arc::new(cfg.handlers);
    let lines: Lines = Arc::new(
        handlers
            .keys()
            .map(|name| (name.clone(), Line::default()))
            .collect::<BTreeMap<_, _>>(),
    );
    for line in lines.values() {
        line.event(Pass::Full, now() - DEBOUNCE); // catching up
    }

    // GitHub's deliveries, where the machine takes them (a host and a secret).
    if let Some(port) = cfg.port {
        let file =
            PathBuf::from(env::var_os("CREDENTIALS_DIRECTORY").unwrap_or_default()).join("webhook");
        let secret = fs::read_to_string(&file)
            .map_err(|e| format!("{}: {e}", file.display()))?
            .trim()
            .to_owned();
        let (h, l) = (handlers.clone(), lines.clone());
        let port = http::listen(port, Arc::new(move |req| delivery(&h, &l, &secret, req)))
            .map_err(|e| format!("deliveries: {e}"))?;
        eprintln!("deliveries on 127.0.0.1:{port}");
    }
    let (h, l) = (handlers.clone(), lines.clone());
    let port = http::listen(cfg.kick_port, Arc::new(move |req| kick(&h, &l, req)))
        .map_err(|e| format!("kicks: {e}"))?;
    let names: Vec<&str> = handlers.keys().map(String::as_str).collect();
    eprintln!("kicks on 127.0.0.1:{port}: {}", names.join(", "));

    for name in handlers.keys() {
        let (name, h, l) = (name.clone(), handlers.clone(), lines.clone());
        thread::spawn(move || serve(&name, &h[&name], &l[&name], &Systemctl, false));
    }
    Ok(())
}
