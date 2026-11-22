//! The stuck role's brief for each reason, as tests/golden keeps it: what
//! a stuck-role job is told, read whole in review. `BLESS=1` rewrites them.
use crate::brief;
use crate::stuck::Ask;
use burrow_brief::Stuck;
use std::path::Path;

#[test]
fn each_reasons_brief_is_its_golden() {
    for why in Stuck::ALL {
        golden(why, Some(40), &format!("stuck-{}.txt", why.word()));
    }
}

/// With no follow-up issue yet, the job is told to open one, and never to
/// look for one.
#[test]
fn the_brief_with_no_follow_up_yet_is_its_golden() {
    golden(Stuck::RoundLimit, None, "stuck-no-followup.txt");
}

fn golden(why: Stuck, followup: Option<u64>, name: &str) {
    let dir = burrow_test::manifest_dir!().join("tests/golden");
    let ask = Ask {
        pr: 7,
        why,
        detail: (why == Stuck::Stall)
            .then(|| "the proof stage stalled twice on one tree:\nGitHub said 422".into()),
    };
    let task = brief::stuck(
        "sam/box",
        7,
        "1111111111111111111111111111111111111111",
        "topic",
        &ask,
        "Review thread on src/lib.rs:3:\nreviewer[bot] wrote:\n> it panics on empty input",
        followup,
        Path::new("/var/lib/proj/jobs"),
    );
    let golden = dir.join(name);
    if std::env::var_os("BLESS").is_some() {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&golden, &task).unwrap();
    }
    let want =
        std::fs::read_to_string(&golden).unwrap_or_else(|e| panic!("{}: {e}", golden.display()));
    assert_eq!(task, want, "{why:?}'s brief isn't {}", golden.display());
}
