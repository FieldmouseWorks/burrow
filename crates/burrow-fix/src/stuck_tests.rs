//! The stuck role's brief for each reason, as tests/golden keeps it: what
//! a stuck-role job is told, read whole in review. `BLESS=1` rewrites them.
use crate::brief;
use crate::stuck::Ask;
use burrow_brief::Stuck;
use std::path::Path;

#[test]
fn each_reasons_brief_is_its_golden() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    for why in Stuck::ALL {
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
            Path::new("/var/lib/proj/jobs"),
        );
        let golden = dir.join(format!("stuck-{}.txt", why.word()));
        if std::env::var_os("BLESS").is_some() {
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(&golden, &task).unwrap();
        }
        let want = std::fs::read_to_string(&golden)
            .unwrap_or_else(|e| panic!("{}: {e}", golden.display()));
        assert_eq!(task, want, "{why:?}'s brief isn't {}", golden.display());
    }
}
