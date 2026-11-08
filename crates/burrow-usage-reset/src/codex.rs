//! Codex's meter, through Codex itself as the machine's user: its
//! app-server's `account/rateLimits/read`, the answer `/status` shows, over
//! the app-server's JSON-RPC on stdin and stdout. No model call, and nothing
//! written to Codex's own files. Its plan's windows count, and so do its
//! credits, which Codex spends by itself once a window is used up: a plan
//! that's out with credits left still has room.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::meter::{Snapshot, Window};

/// How long the app-server has for the whole exchange.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Codex's meter now, read at AT.
pub fn read(codex: &PathBuf, at: f64) -> Result<Snapshot, String> {
    let mut child = Command::new(codex)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("{}: {e}", codex.display()))?;
    let answer = exchange(&mut child);
    let _ = child.kill();
    let _ = child.wait();
    snapshot(&answer?, at)
}

/// The handshake and the one question, on CHILD's stdin and stdout: the
/// result of `account/rateLimits/read`, or why there's none.
fn exchange(child: &mut Child) -> Result<Value, String> {
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    // Lines come from a thread of their own, so a server that says nothing
    // can't hold the pass past the deadline.
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + TIMEOUT;
    let mut send = |message: Value| {
        writeln!(stdin, "{message}")
            .and_then(|()| stdin.flush())
            .map_err(|e| format!("codex app-server stopped listening: {e}"))
    };
    let answer = |id: u64, method: &str| -> Result<Value, String> {
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = rx.recv_timeout(left).map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => {
                    format!("codex app-server didn't answer {method}")
                }
                mpsc::RecvTimeoutError::Disconnected => {
                    format!("codex app-server exited before answering {method}")
                }
            })?;
            // Notifications come between answers: only ours counts.
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message["id"] != id {
                continue;
            }
            if !message["error"].is_null() {
                return Err(format!(
                    "codex app-server refused {method}: {}",
                    message["error"]
                ));
            }
            return Ok(message["result"].clone());
        }
    };
    send(
        json!({"id": 1, "method": "initialize", "params": {"clientInfo": {"name": "burrow-usage-reset", "version": "1"}}}),
    )?;
    answer(1, "initialize")?;
    send(json!({"method": "initialized", "params": {}}))?;
    send(json!({"id": 2, "method": "account/rateLimits/read", "params": {}}))?;
    answer(2, "account/rateLimits/read")
}

/// The meter in an `account/rateLimits/read` RESULT, read at AT. The plan's
/// own bucket (`codex`) is the one that counts: a model's allowance of its
/// own is that model's. An older app-server answers with that bucket alone,
/// as `rateLimits`.
pub fn snapshot(result: &Value, at: f64) -> Result<Snapshot, String> {
    let by_id = &result["rateLimitsByLimitId"];
    let bucket = match by_id.as_object() {
        Some(buckets) if buckets.contains_key("codex") => &buckets["codex"],
        Some(buckets) if buckets.len() == 1 => buckets.values().next().unwrap_or(&Value::Null),
        _ => &result["rateLimits"],
    };
    if !bucket.is_object() {
        return Err(format!(
            "account/rateLimits/read has no plan limits: {result}"
        ));
    }
    let mut windows = std::collections::BTreeMap::new();
    for (slot, window) in [
        ("primary", &bucket["primary"]),
        ("secondary", &bucket["secondary"]),
    ] {
        if window.is_null() {
            continue;
        }
        let used = window["usedPercent"]
            .as_f64()
            .ok_or_else(|| format!("Codex's {slot} window has no usedPercent: {window}"))?;
        // Named by its length, as Codex's /status does: 300m, 10080m.
        let name = window["windowDurationMins"]
            .as_i64()
            .map_or_else(|| slot.to_owned(), |m| format!("{m}m"));
        windows.insert(
            name,
            Window {
                used,
                resets_at: window["resetsAt"].as_f64().unwrap_or(at),
            },
        );
    }
    let credits = &bucket["credits"];
    let credits = credits["hasCredits"] == true || credits["unlimited"] == true;
    if windows.is_empty() && !credits {
        return Err(format!(
            "account/rateLimits/read has neither windows nor credits: {result}"
        ));
    }
    Ok(Snapshot {
        windows,
        credits,
        read_at: at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An answer as Codex's app-server gives it: the $20 plan's two windows,
    /// used up this week, with credits left.
    fn answer() -> Value {
        json!({
            "rateLimits": {"limitId": "codex", "primary": {"usedPercent": 12, "windowDurationMins": 300, "resetsAt": 1_793_000_000}},
            "rateLimitsByLimitId": {
                "codex": {
                    "limitId": "codex",
                    "primary": {"usedPercent": 12, "windowDurationMins": 300, "resetsAt": 1_793_000_000},
                    "secondary": {"usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 1_793_400_000},
                    "credits": {"hasCredits": true, "unlimited": false, "balance": "1234"},
                    "planType": "plus"
                },
                "codex_other": {"limitId": "codex_other", "primary": {"usedPercent": 100, "windowDurationMins": 300}}
            }
        })
    }

    #[test]
    fn the_plans_windows_and_its_credits() {
        let got = snapshot(&answer(), 5.0).unwrap();
        assert_eq!(got.windows.len(), 2);
        assert_eq!(got.windows["300m"].used, 12.0);
        assert_eq!(got.windows["10080m"].used, 100.0);
        assert_eq!(got.windows["10080m"].resets_at, 1_793_400_000.0);
        assert!(got.credits);
        // Out for the week, but its credits carry it on.
        assert!(!got.short(95.0) && got.room());
        let mut spent = answer();
        spent["rateLimitsByLimitId"]["codex"]["credits"]["hasCredits"] = json!(false);
        let spent = snapshot(&spent, 5.0).unwrap();
        assert!(spent.short(95.0) && !spent.room());
        // An older app-server's single bucket.
        let mut old = answer();
        old.as_object_mut().unwrap().remove("rateLimitsByLimitId");
        assert_eq!(snapshot(&old, 5.0).unwrap().windows["300m"].used, 12.0);
        // Signed in with an API key, or nothing to read.
        assert!(snapshot(&json!({}), 5.0).is_err());
        assert!(snapshot(&json!({"rateLimits": {"limitId": "codex"}}), 5.0).is_err());
        assert!(
            snapshot(
                &json!({"rateLimits": {"primary": {"usedPercent": "x"}}}),
                5.0
            )
            .is_err()
        );
    }

    /// A Codex that speaks the app-server's protocol from a script: a
    /// notification ahead of each answer, as the real one sends them.
    fn stand_in(dir: &std::path::Path, limits: &str) -> PathBuf {
        let path = dir.join("codex");
        burrow_test::executable(
            &path,
            format!(
                "#!/bin/sh\n[ \"$1\" = app-server ] || exit 2\nwhile read -r line; do\n  case \"$line\" in\n    *'\"initialize\"'*) echo '{{\"method\":\"note\"}}'; echo '{{\"id\":1,\"result\":{{}}}}' ;;\n    *rateLimits/read*) echo '{{\"method\":\"account/rateLimits/updated\"}}'; echo '{{\"id\":2,\"result\":{limits}}}' ;;\n  esac\ndone\n"
            ),
        );
        path
    }

    #[test]
    fn asked_through_the_app_server() {
        let dir = tempfile::tempdir().unwrap();
        let codex = stand_in(dir.path(), &answer().to_string());
        let got = read(&codex, 7.0).unwrap();
        assert!(got.credits);
        assert_eq!(got.read_at, 7.0);
        let missing = read(&dir.path().join("nothing"), 7.0).unwrap_err();
        assert!(missing.contains("nothing"), "{missing}");
    }
}
