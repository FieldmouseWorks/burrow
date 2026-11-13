use super::authority::{edit_group, group_due, park};
use super::*;

fn due(cfg: &Config, id: &str) -> i64 {
    load(&Job::new(cfg, id)).unwrap().unwrap().due
}

fn near(got: i64, want: i64) {
    assert!(
        (want..=want + 5).contains(&got),
        "got {got}, expected {want}..{}",
        want + 5
    );
}

#[test]
fn restoration_uses_the_selected_deadline_after_merges_and_publication_moves() {
    for legacy in [false, true] {
        for moving in ["merge", "publish"] {
            let (_dir, cfg) = fixture();
            asks(&cfg, "a", Some("source"), false);
            asks(&cfg, "b", Some("destination"), false);
            let old = due(&cfg, "a");
            let selected = crate::now() + 3600;
            edit_group(&cfg, "destination", |g| g["due"] = selected.into());
            let mut rec = park(&cfg, "a", crate::now() - 600, (!legacy).then_some(old));
            let a = Job::new(&cfg, "a");
            if moving == "merge" {
                merge(&cfg, "destination", &["a".into()]).unwrap();
            } else {
                let q = open(&a).unwrap().unwrap();
                about::publish(&a, "destination", q, |q| {
                    fs::write(host_path(&a, "question"), serde_json::to_string(q).unwrap())
                        .map_err(|e| e.to_string())
                })
                .unwrap();
            }
            assert_eq!(due(&cfg, "a"), selected);
            crate::policy::shift_waits(&a, &mut rec).unwrap();
            near(due(&cfg, "a"), selected + 600);
            assert_eq!(due(&cfg, "a"), due(&cfg, "b"));
            crate::cmd::stop(&cfg, "b", false).unwrap();
            merge(&cfg, "fresh", &["a".into()]).unwrap();
            near(due(&cfg, "a"), selected + 600);
        }
    }
}

#[test]
fn missing_deadlines_and_overlapping_members_credit_each_pause_interval_once() {
    for snapshots in [false, true] {
        let (_dir, cfg) = fixture();
        for id in ["a", "b", "c"] {
            asks(&cfg, id, Some("subject"), false);
        }
        let original = group_due(&cfg, "subject");
        let at = crate::now() - 600;
        for id in ["a", "b", "c", "a"] {
            // Every record starts with the same historical pause, even when
            // restoration restarts after its authority commit but before Park.
            let mut rec = park(&cfg, id, at, snapshots.then_some(original));
            crate::policy::shift_waits(&Job::new(&cfg, id), &mut rec).unwrap();
            near(due(&cfg, id), original + 600);
        }
        // A later, non-overlapping pause adds time; an overlapping pause
        // adds only its uncovered portion, independent of restoration order.
        let b = Job::new(&cfg, "b");
        let before = due(&cfg, "b");
        restore(&b, at - 400, at - 200).unwrap();
        assert_eq!(due(&cfg, "b"), before + 200);
        restore(&b, at - 300, at - 100).unwrap();
        assert_eq!(due(&cfg, "b"), before + 300);
    }
}

#[test]
fn restoration_receipts_survive_moves_after_an_interrupted_commit() {
    for occupied in [false, true] {
        let (_dir, cfg) = fixture();
        asks(&cfg, "a", Some("source"), false);
        asks(&cfg, "b", Some("source"), false);
        if occupied {
            asks(&cfg, "dest", Some("destination"), false);
        }
        let original = due(&cfg, "a");
        let selected = original - 1800;
        if occupied {
            edit_group(&cfg, "destination", |g| g["due"] = selected.into());
        }
        let at = crate::now() - 600;
        let a = Job::new(&cfg, "a");
        let mut rec = park(&cfg, "a", at, Some(original));
        fs::write(a.path("await"), "sam/up#1\t1\t{}\n").unwrap();
        // The await rewrite fails once the question's credit is committed:
        // the job's directory refuses the confined writer's replacement file.
        let dir = a.path("await").parent().unwrap().to_path_buf();
        let open = std::os::unix::fs::MetadataExt::mode(&fs::metadata(&dir).unwrap()) & 0o7777;
        burrow_test::permissions(&dir, 0o555);
        let failed = crate::policy::shift_waits(&a, &mut rec).is_err();
        burrow_test::permissions(&dir, open);
        assert!(failed);
        near(due(&cfg, "a"), original + 600);
        // Membership changes before the old restoration can record success.
        merge(&cfg, "destination", &["a".into()]).unwrap();
        let moved = due(&cfg, "a");
        if occupied {
            assert_eq!(moved, selected);
        } else {
            near(moved, original + 600);
        }
        crate::policy::shift_waits(&a, &mut rec).unwrap();
        near(due(&cfg, "a"), moved);
        // A member left on the source also sees the committed group credit.
        let b = Job::new(&cfg, "b");
        restore(&b, at, crate::now()).unwrap();
        near(due(&cfg, "b"), original + 600);
    }
}

#[test]
fn unkeyed_pause_credit_migrates_with_the_question_and_retries_idempotently() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "a", None, false);
    let a = Job::new(&cfg, "a");
    let original = due(&cfg, "a");
    let at = crate::now() - 600;
    restore(&a, at, at + 600).unwrap();
    assert_eq!(due(&cfg, "a"), original + 600);
    restore(&a, at, at + 600).unwrap();
    assert_eq!(due(&cfg, "a"), original + 600);
    merge(&cfg, "subject", &["a".into()]).unwrap();
    restore(&a, at, at + 600).unwrap();
    assert_eq!(due(&cfg, "a"), original + 600);
}
