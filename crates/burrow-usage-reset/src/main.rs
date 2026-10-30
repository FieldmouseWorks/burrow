//! burrow-usage-reset, as modules/usage-reset.nix runs it: one pass, with
//! USAGE_RESET_CONFIG naming its config. It books its own next pass in its
//! wake file, which burrow-events reads.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{env, fs};

use serde::Deserialize;

use burrow_usage_reset::claude::ClaudeCode;
use burrow_usage_reset::now;
use burrow_usage_reset::pass::{FAILED, Notice, State, pass};

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
    let state = match load(&state_file) {
        Ok(state) => state,
        Err(e) => {
            // What it kept may say this week's reset is spent: without it,
            // nothing is pressed.
            tell(
                &cfg.notify,
                Notice::Open(
                    FAILED,
                    "The usage watcher can't read what it kept".to_owned(),
                    format!(
                        "It won't use the banked Claude reset until it can, since what it kept may say this week's is spent: {e}. Look at it, and fix or remove it to start the watcher over."
                    ),
                ),
            );
            return Err(e);
        }
    };
    let dir = cfg.state_dir.join("claude");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut claude = ClaudeCode {
        claude: cfg.claude,
        tmux: cfg.tmux,
        dir,
    };
    let now = now();
    let mut keep = |state: &State| save(&state_file, state);
    let outcome = pass(
        &mut claude,
        &mut keep,
        state,
        cfg.below,
        now,
        &local_time(now),
    );

    save(&state_file, &outcome.state)?;
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

/// What the passes kept in FILE: nothing before the first, and an error for
/// one that can't be read or makes no sense.
fn load(file: &Path) -> Result<State, String> {
    match fs::read(file) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", file.display())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(format!("{}: {e}", file.display())),
    }
}

/// Keep STATE in FILE, on disk before this returns.
fn save(file: &Path, state: &State) -> Result<(), String> {
    write(file, &serde_json::to_vec(state).map_err(|e| e.to_string())?)
}

/// Write FILE whole and to disk: a pass reading it never sees half of it,
/// and a crash just after doesn't lose it.
fn write(file: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = file.with_extension("tmp");
    let dir = file.parent().unwrap_or(Path::new("."));
    fs::File::create(&tmp)
        .and_then(|mut f| {
            std::io::Write::write_all(&mut f, bytes)?;
            f.sync_all()
        })
        .and_then(|()| fs::rename(&tmp, file))
        .and_then(|()| fs::File::open(dir)?.sync_all())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_it_kept_or_nothing_but_never_garbage() {
        let dir = env::temp_dir().join(format!("usage-reset-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("state.json");
        let _ = fs::remove_file(&file);
        assert_eq!(load(&file), Ok(State::default()));
        let kept = State {
            pressed: true,
            ..State::default()
        };
        save(&file, &kept).unwrap();
        assert_eq!(load(&file), Ok(kept));
        // Cut short, or not its own: a spent week mustn't read as a new one.
        fs::write(&file, br#"{"pressed": tr"#).unwrap();
        assert!(load(&file).is_err());
        fs::write(&file, br#"{"pressed": "yes"}"#).unwrap();
        assert!(load(&file).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
