//! Crash metadata and backtraces, without opening the core or exporting its
//! environment, command line, process memory, or other journal fields.

use serde_json::Value;

use super::{Machine, utc};

const FIELDS: [&str; 10] = [
    "COREDUMP_TIMESTAMP",
    "COREDUMP_PID",
    "COREDUMP_UID",
    "COREDUMP_SIGNAL",
    "COREDUMP_SIGNAL_NAME",
    "COREDUMP_EXE",
    "COREDUMP_UNIT",
    "COREDUMP_USER_UNIT",
    "COREDUMP_CGROUP",
    "MESSAGE",
];

pub fn answer(m: &Machine, exe: &str, since: i64) -> Result<String, String> {
    if since >= m.now {
        return Err("the crash span must start before now".into());
    }
    let since_arg = format!("@{since}");
    let until_arg = format!("@{}.999999", m.now);
    let executable = format!("COREDUMP_EXE={exe}");
    let fields = format!("--output-fields={}", FIELDS.join(","));
    let text = m.output(
        &m.cfg.tools.journalctl,
        &[
            "MESSAGE_ID=fc2e22bc6ee647b6b90729ab34a250b1",
            &executable,
            "--since",
            &since_arg,
            "--until",
            &until_arg,
            "--reverse",
            "-n",
            "5",
            "--output=json",
            &fields,
            "--all",
            "--no-pager",
            "-q",
        ],
        false,
        &[],
    )?;
    let mut out = format!(
        "{exe}: latest 5 crashes since {} (newest first)\n",
        utc(since)
    );
    let mut reports = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()).take(5) {
        let entry: Value =
            serde_json::from_str(line).map_err(|e| format!("invalid crash journal entry: {e}"))?;
        // Select again here: journalctl adds mandatory fields even with
        // --output-fields, and none of those belong in this answer.
        if entry.get("COREDUMP_EXE").and_then(Value::as_str) != Some(exe) {
            return Err("crash journal returned a different executable".into());
        }
        reports.push(entry);
    }
    // Keep every report's identity ahead of potentially long backtraces so
    // the job bridge's response limit cannot hide the other crash PIDs.
    for entry in &reports {
        for field in FIELDS.into_iter().filter(|f| *f != "MESSAGE") {
            if let Some(value) = entry.get(field).and_then(Value::as_str) {
                let bounded: String = value.chars().take(1024).collect();
                out += &format!("{field}: {bounded}\n");
                if bounded.len() < value.len() {
                    out += "[field truncated]\n";
                }
            }
        }
        out.push('\n');
    }
    for entry in &reports {
        if let Some(value) = entry.get("MESSAGE").and_then(Value::as_str) {
            let pid = entry
                .get("COREDUMP_PID")
                .and_then(Value::as_str)
                .unwrap_or("?");
            let bounded: String = value.chars().take(12000).collect();
            out += &format!("PID {pid}, MESSAGE: {bounded}\n");
            if bounded.len() < value.len() {
                out += "[field truncated]\n";
            }
        }
    }
    if reports.is_empty() {
        out += "none\n";
    }
    Ok(out)
}
