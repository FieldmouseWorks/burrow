//! Restricted read-only judge execution for Claude Code (`PluginConfig::judge_mode`).
//!
//! Invoked by `burrow-judge`'s look worker inside its isolated `DynamicUser` +
//! `PrivateNetwork` systemd unit: no settings files, no MCP servers, read-only
//! tools alone (`Read,Grep,Glob`), strict JSON schema output, and explicit
//! verification that Claude reported running the requested model in a fresh
//! session.

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

use conary_core::plugin::Context;
use serde::Deserialize;

use super::run::JobSpec;
use super::{JobOutcome, JobUsage, PluginConfig};

/// The read-only tools a judge look is allowed to invoke.
pub const TOOLS: &str = "Read,Grep,Glob";

/// Environment variable naming the read-only look directory (`--add-dir`).
pub const JUDGE_DIR_ENV: &str = "BURROW_JUDGE_DIR";

/// Environment variable carrying the JSON schema for `--json-schema`.
pub const JUDGE_SCHEMA_ENV: &str = "BURROW_JUDGE_JSON_SCHEMA";

/// Claude Code's arguments for a restricted read-only judge look.
pub fn judge_args(
    model: &str,
    effort: Option<&str>,
    schema: &str,
    add_dir: Option<&Path>,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "json",
        "--restricted",
        "--permission-mode",
        "dontAsk",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--tools",
        TOOLS,
        "--json-schema",
        schema,
    ]
    .map(String::from)
    .to_vec();
    if let Some(dir) = add_dir {
        args.extend(["--add-dir".into(), dir.to_string_lossy().into_owned()]);
    }
    args.extend(["--model".into(), model.to_owned()]);
    if let Some(e) = effort {
        args.extend(["--effort".into(), e.to_owned()]);
    }
    args
}

#[derive(Deserialize)]
struct Printed {
    #[serde(default)]
    is_error: bool,
    subtype: Option<String>,
    session_id: Option<String>,
    structured_output: Option<serde_json::Value>,
    #[serde(rename = "modelUsage", default)]
    model_usage: std::collections::BTreeMap<String, serde_json::Value>,
}

/// Run Claude Code in restricted read-only judge mode and validate its
/// structured JSON output, session id, and reported model usage.
pub fn execute(
    config: &PluginConfig,
    spec: &JobSpec<'_>,
    env: &dyn Fn(&str) -> Option<String>,
) -> JobOutcome {
    let fail = |error: String| JobOutcome::Failed {
        session: None,
        error,
        usage: JobUsage::default(),
    };
    if *spec.context != Context::Fresh {
        return fail("judge mode only accepts fresh sessions".to_owned());
    }
    if !spec.workdir.is_dir() {
        return fail(format!("workdir {} does not exist", spec.workdir.display()));
    }
    let Some(model) = spec.model else {
        return fail("judge mode requires an explicit model".to_owned());
    };
    let Some(schema) = env(JUDGE_SCHEMA_ENV).filter(|s| !s.trim().is_empty()) else {
        return fail(format!("judge mode requires {JUDGE_SCHEMA_ENV}"));
    };
    let cred_var = config.credential_env();
    let Some(token) = env(cred_var).filter(|v| !v.trim().is_empty()) else {
        return fail(format!("no login credential is set in {cred_var}"));
    };
    let add_dir = env(JUDGE_DIR_ENV)
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from);
    let args = judge_args(model, spec.effort, &schema, add_dir.as_deref());

    let mut cmd = Command::new(config.program());
    cmd.args(&args)
        .current_dir(spec.workdir)
        .env_clear()
        .env("CLAUDE_CODE_OAUTH_TOKEN", token.trim())
        .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for key in [
        "PATH",
        "HOME",
        "TMPDIR",
        "HTTPS_PROXY",
        "https_proxy",
        "NO_PROXY",
        "no_proxy",
    ] {
        if let Some(val) = env(key) {
            cmd.env(key, val);
        }
    }
    if let Some(base_url) = &config.api_base_url {
        cmd.env("ANTHROPIC_BASE_URL", base_url);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("{}: {e}", config.program().display())),
    };
    let Some(mut stdin) = child.stdin.take() else {
        return fail("Claude's stdin unavailable".to_owned());
    };
    let prompt = spec.prompt.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
    let out = match child.wait_with_output() {
        Ok(o) => o,
        Err(e) => {
            let _ = writer.join();
            return fail(format!("{}: {e}", config.program().display()));
        }
    };
    let _ = writer.join();

    if !out.status.success() {
        let why = parse_output(&out.stdout, model)
            .err()
            .unwrap_or_else(|| "Claude exited unsuccessfully".to_owned());
        return fail(format!("{why} (it exited {})", out.status));
    }
    match parse_output(&out.stdout, model) {
        Ok((session, answer)) => JobOutcome::Ok {
            session: Some(session),
            answer,
            usage: JobUsage::default(),
        },
        Err(why) => fail(format!("{why} (it exited {})", out.status)),
    }
}

fn parse_output(printed: &[u8], expected_model: &str) -> Result<(String, String), String> {
    let p: Printed = serde_json::from_slice(printed)
        .map_err(|e| format!("Claude printed no result that reads: {e}"))?;
    if p.is_error || p.subtype.as_deref() != Some("success") {
        return Err(format!(
            "Claude's run failed ({})",
            p.subtype.as_deref().unwrap_or("no result")
        ));
    }
    let said = p
        .structured_output
        .ok_or("Claude gave no structured output")?;
    let session = p
        .session_id
        .filter(|s| !s.trim().is_empty())
        .ok_or("Claude's run names no session")?;
    if !p.model_usage.contains_key(expected_model) {
        return Err(format!(
            "Claude ran {:?}, not {expected_model}",
            p.model_usage.keys().collect::<Vec<_>>()
        ));
    }
    let answer = serde_json::to_string(&said).map_err(|e| e.to_string())?;
    Ok((session, answer))
}
