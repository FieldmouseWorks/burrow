//! burrow-usage-reset, as modules/usage-reset.nix runs it: one pass, with
//! USAGE_RESET_CONFIG naming its config. It books its own next pass in its
//! wake file, which burrow-events reads.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

use serde::Deserialize;

use burrow_usage_reset::claude::ClaudeCode;
use burrow_usage_reset::now;
use burrow_usage_reset::pass::{Notice, State, pass};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Config {
    /// The `claude` to run.
    claude: PathBuf,
    /// A tmux that keeps its panes in this unit.
    tmux: PathBuf,
    /// The reset is used once the week has less than this percent left.
    below: f64,
    /// Where the passes keep their state and wake file.
    state_dir: PathBuf,
    /// HOST-notify, or "" for the journal only.
    notify: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("burrow-usage-reset: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let path = env::var_os("USAGE_RESET_CONFIG").ok_or("USAGE_RESET_CONFIG isn't set")?;
    let text = fs::read(&path).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;
    let cfg: Config =
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.to_string_lossy()))?;

    let state_file = cfg.state_dir.join("state.json");
    let state: State = fs::read(&state_file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let dir = cfg.state_dir.join("claude");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut claude = ClaudeCode {
        claude: cfg.claude,
        tmux: cfg.tmux,
        dir,
    };
    let now = now();
    let outcome = pass(&mut claude, state, cfg.below, now, &local_time(now));

    write(
        &state_file,
        &serde_json::to_vec(&outcome.state).map_err(|e| e.to_string())?,
    )?;
    write(
        &cfg.state_dir.join("wake.json"),
        serde_json::json!({ "": outcome.wake.round() })
            .to_string()
            .as_bytes(),
    )?;
    for notice in outcome.notices {
        tell(&cfg.notify, notice);
    }
    Ok(())
}

/// Write FILE whole: a pass reading it never sees half of it.
fn write(file: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, bytes)
        .and_then(|()| fs::rename(&tmp, file))
        .map_err(|e| format!("{}: {e}", file.display()))
}

/// Hand a notice to HOST-notify, which never fails its caller; without it,
/// the journal has it.
fn tell(notify: &str, notice: Notice) {
    let args = match &notice {
        Notice::Open(key, title, body) => vec!["open", key, title, body],
        Notice::Close(key, how) => vec!["close", key, how],
    };
    if notify.is_empty() {
        eprintln!("{}", args.join(": "));
        return;
    }
    if let Err(e) = Command::new(notify).args(&args).status() {
        eprintln!("{notify}: {e}");
    }
}

/// A moment as the machine's clock (its time zone) says it, for the owner.
fn local_time(t: f64) -> String {
    Command::new("date")
        .args(["-d", &format!("@{}", t.round()), "+%a %-I:%M %p %Z"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_else(|| format!("{} (epoch)", t.round()))
}
