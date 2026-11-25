//! Conary `conary-plugin v5` executable for Antigravity: reads one JSON
//! request from stdin, writes one JSON response to stdout, and logs failures on
//! stderr.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use burrow_engine_antigravity::{Config, normalize, plugin};
use conary_core::plugin::{Request, Task};

/// Says `activity` on stderr every `every` until dropped: the CLI's output
/// comes only once it ends, so a runner's idle limit would take a long, live
/// run for a quiet one. A hung CLI is still bounded by `deadline_secs`.
struct Activity {
    stop: Option<mpsc::Sender<()>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Activity {
    fn start(every: Duration) -> Self {
        let (stop, stopped) = mpsc::channel::<()>();
        let thread = thread::spawn(move || {
            while let Err(mpsc::RecvTimeoutError::Timeout) = stopped.recv_timeout(every) {
                eprintln!(r#"{{"event":"activity"}}"#);
            }
        });
        Self {
            stop: Some(stop),
            thread: Some(thread),
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config_path = match args.as_slice() {
        [] => None,
        [path] => Some(PathBuf::from(path)),
        _ => {
            eprintln!(
                "burrow-engine-antigravity: usage: burrow-engine-antigravity [CONFIG_JSON] (speaks conary-plugin v5 on stdio)"
            );
            return ExitCode::from(plugin::EXIT_FAILURE);
        }
    };
    let config = match Config::load(config_path.as_deref()) {
        Ok(c) => c,
        Err(why) => {
            eprintln!("burrow-engine-antigravity: {why}");
            return ExitCode::from(plugin::EXIT_FAILURE);
        }
    };
    let mut input = Vec::new();
    if let Err(e) = io::stdin().lock().read_to_end(&mut input) {
        eprintln!("burrow-engine-antigravity: failed to read plugin request: {e}");
        return ExitCode::from(plugin::EXIT_FAILURE);
    }
    let mut request: Request = match serde_json::from_slice(&input) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("burrow-engine-antigravity: failed to decode plugin request: {e}");
            return ExitCode::from(plugin::EXIT_FAILURE);
        }
    };
    plugin::add_instructions(
        &mut request,
        std::env::var("PROJ_APPEND_SYSTEM_PROMPT").ok().as_deref(),
    );
    let streams = std::env::var("PROJ_STREAM_EVENTS").is_ok_and(|v| v == "1");
    let stream_job = streams && matches!(request.task, Task::Job { .. });
    let activity = stream_job.then(|| Activity::start(Duration::from_secs(config.activity_secs)));
    let mut emitted_session = false;
    let mut on_line = |line: &str| {
        if !stream_job {
            return;
        }
        if let Ok(event) = serde_json::to_string(&serde_json::json!({
            "event": "stream",
            "line": line,
        })) {
            eprintln!("{event}");
        }
        if !emitted_session
            && let Some(sid) = normalize::extract_session_from_line(line)
            && let Ok(event) = serde_json::to_string(&serde_json::json!({
                "event": "session",
                "session_id": sid,
            }))
        {
            emitted_session = true;
            eprintln!("{event}");
        }
    };
    let result = plugin::handle_request_streaming(&config, &request, &|| false, &mut on_line);
    drop(activity);
    if let Some(diag) = &result.diagnostic
        && let Ok(line) = serde_json::to_string(diag)
    {
        eprintln!("{line}");
    }
    if let Some(response) = &result.response {
        let written = serde_json::to_vec(response)
            .map_err(|e| format!("failed to encode plugin response: {e}"))
            .and_then(|bytes| {
                let mut out = io::stdout().lock();
                out.write_all(&bytes)
                    .and_then(|()| out.write_all(b"\n"))
                    .and_then(|()| out.flush())
                    .map_err(|e| format!("failed to write plugin response: {e}"))
            });
        if let Err(why) = written {
            eprintln!("burrow-engine-antigravity: {why}");
            return ExitCode::from(plugin::EXIT_FAILURE);
        }
    }
    ExitCode::from(result.exit_code)
}
