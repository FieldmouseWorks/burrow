//! The v2 issuance: generations allocated past every one before and kept
//! first, and a check that reads as Conary's flow does. That the flow
//! itself counts what the sweep hands out is tests/sweep/conary.rs's.

use std::fs;
use std::path::PathBuf;

use conary_github::flow::{Revision, SealKey};

use super::{Issuance, Read, generations};
use crate::sweep::seal::KEY;

const AB: &str = "abababababababababababababababababababababababababababababababab";

fn state(key: Option<&str>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("renovate-sweep");
    fs::create_dir(&state).unwrap();
    if let Some(k) = key {
        fs::write(state.join(KEY), k).unwrap();
    }
    (dir, state)
}

fn revision(head: u64, description: &str) -> Revision {
    Revision::for_parts(
        format!("{head:040x}"),
        "main".into(),
        "Update x to v2",
        description,
    )
}

/// What's between the marker's `<!-- major-verdict:v2 ` and ` -->`.
fn payload(marker: &str) -> String {
    marker
        .strip_prefix("<!-- major-verdict:v2 ")
        .and_then(|m| m.strip_suffix(" -->"))
        .unwrap()
        .to_owned()
}

fn read(state: &std::path::Path, r: &Revision, markers: &[&String]) -> Read {
    let payloads: Vec<String> = markers.iter().map(|m| payload(m)).collect();
    Issuance {
        repo: "sam/r",
        n: 3,
        revision: r,
    }
    .read(state, &payloads)
    .unwrap()
}

/// Each marker is Conary's own decision, byte for byte, on a generation of
/// its own: ok's, then held's, kept in the state dir before they're
/// returned, and the next pair past both, past what's posted, and past what
/// was kept for the repo in any case.
#[test]
fn each_pair_is_conarys_decision_on_new_generations() {
    let (_d, st) = state(Some(AB));
    let r = revision(1, "body");
    let i = Issuance {
        repo: "Sam/R",
        n: 3,
        revision: &r,
    };
    let [ok, held] = i.markers(&st, 0).unwrap();
    let key = SealKey::from_hex(AB).unwrap();
    assert_eq!(ok, key.decision("sam/r", 3, "major-verdict", "ok", &r, 1));
    assert_eq!(
        held,
        key.decision("sam/r", 3, "major-verdict", "held", &r, 2)
    );
    let kept = generations(&st, "sam/r", 3);
    assert_eq!(fs::read_to_string(&kept).unwrap(), "2\n");
    let [ok, _] = i.markers(&st, 0).unwrap();
    assert!(ok.contains(r#""generation":3"#), "{ok}");
    // Past what's posted, when that's further on (a state dir restored
    // from before it).
    let [ok, held] = i.markers(&st, 10).unwrap();
    assert!(ok.contains(r#""generation":11"#), "{ok}");
    assert!(held.contains(r#""generation":12"#), "{held}");
    assert_eq!(fs::read_to_string(&kept).unwrap(), "12\n");
    // Nowhere left to go: nothing sealed, nothing kept.
    assert!(i.markers(&st, u64::MAX - 1).is_err());
    assert_eq!(fs::read_to_string(&kept).unwrap(), "12\n");
    // A kept generation that isn't one seals nothing: it would start over.
    fs::write(&kept, "twelve").unwrap();
    assert!(i.markers(&st, 0).is_err());
}

/// The verdict is the latest generation's on this revision, as Conary
/// reads it: whatever came before it, only when it says one thing, and
/// only as the sweep sealed it.
#[test]
fn the_latest_generation_on_the_revision_is_the_verdict() {
    let (_d, st) = state(Some(AB));
    let r = revision(1, "body");
    let i = Issuance {
        repo: "sam/r",
        n: 3,
        revision: &r,
    };
    let [ok1, held1] = i.markers(&st, 0).unwrap();
    let [ok2, _] = i.markers(&st, 0).unwrap();
    let said = |v: Option<&str>, highest| Read {
        verdict: v.map(str::to_owned),
        highest,
    };
    assert_eq!(read(&st, &r, &[]), said(None, 0));
    assert_eq!(read(&st, &r, &[&ok1]), said(Some("ok"), 1));
    assert_eq!(read(&st, &r, &[&ok1, &held1]), said(Some("held"), 2));
    assert_eq!(read(&st, &r, &[&held1, &ok1]), said(Some("held"), 2));
    // A later handover's ok overtakes the earlier held, posted after it or
    // not.
    assert_eq!(read(&st, &r, &[&ok2, &held1]), said(Some("ok"), 3));
    // Another revision's count toward what's been sealed, never as its
    // verdict: a push, a retarget or a description edit since.
    for other in [
        revision(2, "body"),
        Revision {
            base: "release".into(),
            ..r.clone()
        },
        revision(1, "body, edited"),
    ] {
        assert_eq!(read(&st, &other, &[&ok1]), said(None, 1));
    }
    // Under another key, edited, or as another PR: none at all.
    let (_d, other) = state(Some(&"cd".repeat(32)));
    assert_eq!(read(&other, &r, &[&ok1]), said(None, 0));
    let edited = ok1.replace(r#""verdict":"ok""#, r#""verdict":"held""#);
    assert_eq!(read(&st, &r, &[&edited]), said(None, 0));
    let spaced = ok1.replace(r#"{"generation""#, r#"{ "generation""#);
    assert_eq!(read(&st, &r, &[&spaced]), said(None, 0));
    let elsewhere = Issuance {
        repo: "sam/r",
        n: 4,
        revision: &r,
    };
    assert_eq!(
        elsewhere.read(&st, &[payload(&ok1)]).unwrap(),
        said(None, 0)
    );
    // Two words on the latest generation (a key in two hands) are none.
    let key = SealKey::from_hex(AB).unwrap();
    let both = [
        key.decision("sam/r", 3, "major-verdict", "ok", &r, 9),
        key.decision("sam/r", 3, "major-verdict", "held", &r, 9),
    ];
    assert_eq!(read(&st, &r, &[&ok1, &both[0], &both[1]]), said(None, 9));
    // A word that's neither, latest, is none either, and overtakes.
    let odd = key.decision("sam/r", 3, "major-verdict", "maybe", &r, 10);
    assert_eq!(read(&st, &r, &[&ok1, &odd]), said(None, 10));
    // Generation 0 is never one.
    let zero = key.decision("sam/r", 3, "major-verdict", "ok", &r, 0);
    assert_eq!(read(&st, &r, &[&zero]), said(None, 0));
}

/// A check with no key reads nothing and makes none.
#[test]
fn a_check_never_makes_a_key() {
    let (_d, st) = state(None);
    let r = revision(1, "body");
    let marker = SealKey::from_hex(AB)
        .unwrap()
        .decision("sam/r", 3, "major-verdict", "ok", &r, 1);
    assert_eq!(read(&st, &r, &[&marker]), Read::default());
    assert!(!st.join(KEY).exists());
}
