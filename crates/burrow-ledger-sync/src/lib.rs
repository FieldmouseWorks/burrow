//! The ledger's pass, `burrow-ledger-sync CONFIG [OWNER/REPO]`: every repo
//! of the machine's owners, or the one it's given, brought up to date in
//! Conary's ledger by one `cn sync`, which reads the ledger's log once for
//! them all and lets its lock go between them (`cn sync --help` says how it
//! answers). modules/ledger.nix runs it with the ledger's writer's
//! environment, and its `gh` and `cn` on PATH.
//!
//! A pass fails for what retrying won't fix, and only that: a failed unit
//! is what fails a switch that runs meanwhile. So an owner whose repos
//! can't be listed, or a repo that fails, fails it without holding up the
//! rest; a repo GitHub couldn't be asked about (its rate limit, its errors,
//! the network) is booked for a pass five minutes on instead, and fails it
//! only when that booking can't be written.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Map, Value};

/// A line on stderr in one write, so it isn't interleaved with cn's own,
/// which goes to the same journal: `eprintln!` writes each piece apart.
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = std::io::stderr().write_all(format!("{}\n", format_args!($($arg)*)).as_bytes());
    }};
}

/// What modules/ledger.nix tells the pass.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Whose repos a full pass syncs: every one `gh repo list` gives for
    /// each, archived ones and forks left out.
    pub owners: Vec<String>,
    /// When each repo's next pass is due, for burrow-events
    /// (`{"OWNER/REPO": epoch}`).
    pub wake: PathBuf,
}

/// cn's exit when GitHub couldn't be asked just now.
const UNAVAILABLE: i32 = 75;

/// How long a repo GitHub was unavailable for waits for its next pass.
const AGAIN: u64 = 300;

/// How one repo's sync ended, as `cn sync` says it on a line of its own.
#[derive(Deserialize)]
struct Ended {
    repo: String,
    outcome: Outcome,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
enum Outcome {
    Done,
    Unavailable,
    Failed,
}

/// Runs the pass over `only`, or over every owner's repos, the ledger being
/// `ledger` (cn's `CONARY_LEDGER`). Whether it passed; what didn't goes to
/// stderr.
pub fn run(config: &Config, ledger: &Path, only: Option<String>) -> bool {
    // Sets the ledger up, until set-up has finished: its last step keeps
    // the anchor's copy beside the log. Run on a ledger that's set up, it
    // checks the anchor's whole history on GitHub, a call per commit on it,
    // and every sync adds one. A ledger whose key was lost fails here until
    // the key is back, and sync refuses one whose anchor moved.
    if !ledger.join("anchor").exists() {
        match Command::new("cn")
            .arg("init")
            .stdout(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => {
                say!("cn init: {status}");
                return false;
            }
            Err(e) => {
                say!("cn init: {e}");
                return false;
            }
        }
    }
    let mut passed = true;
    let repos = match only {
        Some(repo) => vec![repo],
        None => {
            let mut all = Vec::new();
            for owner in &config.owners {
                match list(owner) {
                    Ok(repos) => all.extend(repos),
                    Err(e) => {
                        say!("couldn't list {owner}'s repos: {e}");
                        passed = false;
                    }
                }
            }
            all
        }
    };
    // cn refuses a sync of no repos: there's nothing to ask it.
    if repos.is_empty() {
        return passed;
    }
    sync(&config.wake, &repos) && passed
}

/// An owner's repos, as `gh` lists them.
fn list(owner: &str) -> Result<Vec<String>, String> {
    let out = Command::new("gh")
        .args([
            "repo",
            "list",
            owner,
            "--limit",
            "1000",
            "--no-archived",
            "--source",
        ])
        .args(["--json", "nameWithOwner", "--jq", ".[].nameWithOwner"])
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(format!("gh repo list: {}", out.status));
    }
    let text = String::from_utf8(out.stdout).map_err(|e| format!("gh repo list: {e}"))?;
    Ok(text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// One `cn sync` of `repos`: each repo's line is read as it comes, so a
/// pass killed partway keeps the bookings it made. A repo it never said a
/// line for takes its exit's word: 75, GitHub was unavailable before it got
/// to it, else it failed.
fn sync(wake: &Path, repos: &[String]) -> bool {
    // What a pass's journal is read by for how many calls it made.
    say!("syncing {} repos in one cn sync", repos.len());
    let mut child = match Command::new("cn")
        .arg("sync")
        .args(repos)
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            say!("cn sync: {e}");
            return false;
        }
    };
    let mut passed = true;
    let mut ended = BTreeSet::new();
    let mut booked = false;
    let mut not_done = false;
    if let Some(out) = child.stdout.take() {
        for line in BufReader::new(out).lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    say!("cn sync's output: {e}");
                    passed = false;
                    break;
                }
            };
            // The journal keeps each repo's line, as when cn wrote it there.
            println!("{line}");
            let one = match serde_json::from_str::<Ended>(&line) {
                Ok(one) if repos.contains(&one.repo) => one,
                Ok(one) => {
                    say!(
                        "cn sync said how {} ended, which it wasn't asked to sync",
                        one.repo
                    );
                    passed = false;
                    continue;
                }
                Err(e) => {
                    say!("cn sync said what isn't how a repo ended ({e})");
                    passed = false;
                    continue;
                }
            };
            ended.insert(one.repo.clone());
            not_done |= one.outcome != Outcome::Done;
            match one.outcome {
                Outcome::Done => {}
                Outcome::Unavailable => {
                    booked = true;
                    passed &= again(wake, &one.repo);
                }
                Outcome::Failed => {
                    let why = one.error.as_deref().unwrap_or("no error given");
                    say!("couldn't sync {}: {why}", one.repo);
                    passed = false;
                }
            }
        }
    }
    let status = match child.wait() {
        Ok(status) => Some(status),
        Err(e) => {
            say!("cn sync: {e}");
            None
        }
    };
    let code = status.and_then(|s| s.code());
    let said = |status: Option<ExitStatus>| status.map_or("no exit".into(), |s| s.to_string());
    for repo in repos.iter().filter(|r| !ended.contains(*r)) {
        if code == Some(UNAVAILABLE) {
            booked = true;
            passed &= again(wake, repo);
        } else {
            say!(
                "couldn't sync {repo}: cn sync ended ({}) without saying how it ended",
                said(status)
            );
            passed = false;
        }
    }
    // Its lines and its exit agree, or neither can be trusted.
    match code {
        Some(0) if !not_done => {}
        Some(UNAVAILABLE) if booked => {}
        _ => {
            say!("cn sync: {}", said(status));
            passed = false;
        }
    }
    passed
}

/// Books `repo`'s next pass five minutes on; whether that was written.
fn again(wake: &Path, repo: &str) -> bool {
    say!("{repo}: GitHub is unavailable; again in five minutes");
    let at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
        + AGAIN;
    match book(wake, repo, at) {
        Ok(()) => true,
        Err(e) => {
            say!(
                "couldn't book {repo}'s next pass in {}: {e}",
                wake.display()
            );
            false
        }
    }
}

/// Books a repo's next pass at a time. Every other repo's booking stays,
/// however overdue: burrow-events reads the file only once this pass ends,
/// and takes a booking only if it came after that repo's last pass started,
/// so one already seen to is no pass. A file that isn't exactly one object
/// is left as it is, and the booking fails.
pub fn book(wake: &Path, repo: &str, at: u64) -> io::Result<()> {
    let lock = OpenOptions::new()
        .create(true)
        .append(true)
        .open(beside(wake, ".lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    let mut booked = match fs::read_to_string(wake) {
        Ok(text) => serde_json::from_str::<Map<String, Value>>(&text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("not one {{repo: epoch}}: {e}"),
            )
        })?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(e),
    };
    booked.insert(repo.to_string(), at.into());
    let new = beside(wake, ".new");
    let mut text = serde_json::to_string(&booked).map_err(io::Error::other)?;
    text.push('\n');
    fs::write(&new, text)?;
    fs::rename(&new, wake)?;
    drop::<File>(lock);
    Ok(())
}

/// `path` with `suffix` after its name.
fn beside(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}
