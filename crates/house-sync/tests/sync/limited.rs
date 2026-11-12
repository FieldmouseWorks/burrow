//! GitHub's rate limit holds a pass off: each owner's installation has an
//! allowance of its own, so only the owner it holds off is left, the pass
//! is booked again in the house handler's wake file for when the first
//! limit lifts, and it exits 0, so a spent allowance never fails the unit.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::github::{Fails, Repo, repo};
use super::{Setup, setup, stale, sync_with};

/// When the stand-in's rate limits lift, in seconds since the epoch.
const SOON: u64 = 4_000_000_000;
const LATER: u64 = 4_000_003_600;

fn booked(wake: &Path) -> Value {
    serde_json::from_slice(&fs::read(wake).unwrap()).unwrap()
}

fn failing(name: &str, fails: Fails) -> Repo {
    Repo {
        fails,
        ..repo(name, Some(&stale()))
    }
}

#[test]
fn a_repos_pass_the_rate_limit_holds_off_is_booked_for_when_it_lifts() {
    let dir = tempfile::tempdir().unwrap();
    let s = Setup {
        wake: dir.path().join("wake.json"),
        ..setup()
    };
    let held = [failing("Sam/A", Fails::Limited(SOON))];
    let ran = sync_with(&held, Some("Sam/A"), &s);
    assert_eq!(ran.ok, Ok(true));
    assert!(ran.wrote().is_empty() && ran.told.is_empty());
    assert_eq!(booked(&s.wake), json!({"sam/a": SOON}));
    assert!(
        ran.out.contains(&format!(
            "house-sync: Sam/A: held off by GitHub's rate limit until {SOON}, booked again (GET repos/Sam/A: HTTP 403: API rate limit exceeded"
        )) && ran.out.contains(&format!("house-sync: Sam/A: booked again for {SOON}")),
        "{}",
        ran.out
    );
    // A dry pass books nothing: it's the failure it was.
    let dry = Setup {
        dry_run: true,
        wake: dir.path().join("dry.json"),
        ..setup()
    };
    let ran = sync_with(&held, Some("Sam/A"), &dry);
    assert_eq!(ran.ok, Ok(false));
    assert!(!dry.wake.exists());
    // Once the limit has lifted, the pass that gets through uses its
    // booking up, and leaves the others'.
    fs::write(&s.wake, json!({"sam/a": 1, "sam/b": SOON}).to_string()).unwrap();
    let ran = sync_with(&[repo("Sam/A", Some(&stale()))], Some("Sam/A"), &s);
    assert_eq!(ran.ok, Ok(true));
    assert_eq!(ran.wrote(), ["Sam/A"]);
    assert_eq!(booked(&s.wake), json!({"sam/b": SOON}));
}

#[test]
fn a_full_pass_leaves_only_the_owners_the_rate_limit_holds_off() {
    let dir = tempfile::tempdir().unwrap();
    let s = Setup {
        wake: dir.path().join("wake.json"),
        owners: &["Sam", "Eve", "Ann"],
        ..setup()
    };
    // Sam's repo list is held off on its second page, until later. Eve's
    // installation runs out on her first repo, until sooner, and the rest
    // of hers would only be held off too. Ann's own allowance is hers: her
    // repos go on, one of them failing as any repo can.
    let repos = [
        repo("sam/a", Some(&stale())),
        repo("sam/b", Some(&stale())),
        failing("sam/c", Fails::ListLimited(LATER)),
        failing("eve/x", Fails::Limited(SOON)),
        repo("eve/y", Some(&stale())),
        repo("ann/z", Some(&stale())),
        failing("ann/broken", Fails::Below),
    ];
    let ran = sync_with(&repos, None, &s);
    assert_eq!(ran.ok, Ok(true));
    assert_eq!(ran.listed, ["Sam", "Sam", "Eve", "Ann"]);
    assert_eq!(ran.wrote(), ["ann/z"]);
    assert!(
        ran.asked.iter().all(|(_, p, _)| !p.contains("eve/y")),
        "{:?}",
        ran.asked
    );
    // Again when the first limit lifts.
    assert_eq!(booked(&s.wake), json!({"": SOON}));
    assert!(
        ran.out
            .contains(&format!("house-sync: every repo: booked again for {SOON}")),
        "{}",
        ran.out
    );
    // The failure is news; an all-clear isn't, from a pass that didn't
    // reach every repo.
    assert_eq!(
        ran.told
            .iter()
            .map(|t| [t[0].as_str(), t[1].as_str()])
            .collect::<Vec<_>>(),
        [["open", "house-sync"]]
    );
    assert!(
        ran.told[0][3].contains(": ann/broken. It"),
        "{:?}",
        ran.told
    );
    // Held off with nothing failing, it tells the owner nothing.
    fs::remove_file(&s.wake).unwrap();
    let ran = sync_with(&repos[..6], None, &s);
    assert_eq!(ran.ok, Ok(true));
    assert!(ran.told.is_empty(), "{:?}", ran.told);
    assert_eq!(booked(&s.wake), json!({"": SOON}));
}
