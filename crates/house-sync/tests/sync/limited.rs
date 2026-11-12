//! GitHub's rate limit holds a pass off: it's booked again in the house
//! handler's wake file for when the limit lifts, and the pass exits 0 with
//! nothing for the owner, so a spent allowance never fails the unit.

use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::github::{Fails, RESET, Repo, repo};
use super::{Setup, setup, stale, sync_with};

fn booked(wake: &Path) -> Value {
    serde_json::from_slice(&fs::read(wake).unwrap()).unwrap()
}

#[test]
fn a_repos_pass_the_rate_limit_holds_off_is_booked_for_when_it_lifts() {
    let dir = tempfile::tempdir().unwrap();
    let s = Setup {
        wake: dir.path().join("wake.json"),
        ..setup()
    };
    let held = [Repo {
        fails: Fails::Limited,
        ..repo("Sam/A", Some(&stale()))
    }];
    let ran = sync_with(&held, Some("Sam/A"), &s);
    assert_eq!(ran.ok, Ok(true));
    assert!(ran.wrote().is_empty() && ran.told.is_empty());
    assert_eq!(booked(&s.wake), json!({"sam/a": RESET}));
    assert!(
        ran.out.contains(&format!(
            "house-sync: Sam/A: booked again for {RESET}, when GitHub's rate limit lifts (GET repos/Sam/A: HTTP 403: API rate limit exceeded"
        )),
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
    fs::write(&s.wake, json!({"sam/a": 1, "sam/b": RESET}).to_string()).unwrap();
    let ran = sync_with(&[repo("Sam/A", Some(&stale()))], Some("Sam/A"), &s);
    assert_eq!(ran.ok, Ok(true));
    assert_eq!(ran.wrote(), ["Sam/A"]);
    assert_eq!(booked(&s.wake), json!({"sam/b": RESET}));
}

#[test]
fn a_full_pass_the_rate_limit_holds_off_stops_and_is_booked_whole() {
    let dir = tempfile::tempdir().unwrap();
    let s = Setup {
        wake: dir.path().join("wake.json"),
        ..setup()
    };
    // A repo it holds off: the rest would only be held off too, so the
    // pass stops there, and the notices stay as they were.
    let held = [
        Repo {
            fails: Fails::Limited,
            ..repo("sam/a", Some(&stale()))
        },
        repo("sam/b", Some(&stale())),
    ];
    let ran = sync_with(&held, None, &s);
    assert_eq!(ran.ok, Ok(true));
    assert!(ran.read().is_empty() && ran.wrote().is_empty());
    assert!(ran.told.is_empty(), "{:?}", ran.told);
    assert_eq!(booked(&s.wake), json!({"": RESET}));
    assert!(
        ran.out
            .contains(&format!("house-sync: every repo: booked again for {RESET}")),
        "{}",
        ran.out
    );
    // So is an owner whose repos it can't list.
    fs::remove_file(&s.wake).unwrap();
    let listed = [Repo {
        fails: Fails::ListLimited,
        ..repo("sam/a", Some(&stale()))
    }];
    let ran = sync_with(&listed, None, &s);
    assert_eq!(ran.ok, Ok(true));
    assert!(ran.read().is_empty() && ran.told.is_empty());
    assert_eq!(booked(&s.wake), json!({"": RESET}));
}
