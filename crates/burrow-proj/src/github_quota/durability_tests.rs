use super::tests::fixture;
use super::*;
use std::os::unix::fs::symlink;

fn job(cfg: &Config) -> Job<'_> {
    let job = Job::new(cfg, "durability");
    fs::create_dir_all(&job.dir).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();
    job.set_turns(1).unwrap();
    job
}
fn ask(job: &Job) {
    crate::ask::record(
        job,
        &crate::ask::Recommendation {
            default: "Keep the archive".into(),
            one_way: true,
        },
    )
    .unwrap();
}

#[test]
fn declaration_publication_is_confined_against_staging_and_parent_aliases() {
    let (d, cfg) = fixture();
    let job = job(&cfg);
    let outside = d.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("value"), "untouched").unwrap();
    for name in ["ask.next.new", "end.next.new"] {
        symlink(outside.join("value"), job.path(name)).unwrap();
    }
    ask(&job);
    crate::ask::declare(&job, "ask").unwrap();
    assert_eq!(
        fs::read_to_string(outside.join("value")).unwrap(),
        "untouched"
    );
    assert_eq!(read(&job.path("end.next")).as_deref(), Some("ask"));
    let kept = cfg.jobs.join("retained");
    fs::rename(&job.dir, &kept).unwrap();
    symlink(&outside, &job.dir).unwrap();
    assert!(crate::ask::declare(&job, "done").is_err());
    assert!(!outside.join("end.next").exists());
}

#[test]
fn quota_cleanup_unlink_failures_preserve_the_superseding_marker() {
    for end in ["done", "await"] {
        let (_d, cfg) = fixture();
        let job = job(&cfg);
        // A directory makes unlink fail deterministically, even as root.
        fs::create_dir(job.path("ask.next")).unwrap();
        fs::write(job.path("end.next"), end).unwrap();
        assert!(park(&job, 1).is_err(), "cleanup ignored an unlink failure");
        assert_eq!(read(&job.path("end.next")).as_deref(), Some(end));
        assert!(crate::ask::pending(&job).unwrap().is_none());
        assert!(crate::ask::load(&job).is_none());
    }
}

#[test]
fn all_safe_cleanup_intermediate_states_remain_retired_during_recovery() {
    for end in ["done", "await"] {
        for removed in [0, 1, 2] {
            for recovery in ["finish", "watch", "replay", "restore"] {
                let (_d, cfg) = fixture();
                let job = job(&cfg);
                ask(&job);
                fs::write(job.path("end.next"), end).unwrap();
                // Before cleanup, after retiring the recommendation, and
                // after retiring its marker: no state can reactivate an ask.
                if removed > 0 {
                    fs::remove_file(job.path("ask.next")).unwrap();
                }
                if removed > 1 {
                    fs::remove_file(job.path("end.next")).unwrap();
                }
                fs::write(
                    job.path(PENDING),
                    serde_json::to_vec(&Pending {
                        targets: vec!["sam/r#7".into()],
                        retry_at: 1,
                        turn: "1".into(),
                        baselines: String::new(),
                    })
                    .unwrap(),
                )
                .unwrap();
                fs::write(job.turn("1", "outcome"), "failed").unwrap();
                match recovery {
                    "finish" => {
                        finish(&job, "1").unwrap();
                        finish(&job, "1").unwrap();
                    }
                    "watch" => {
                        crate::watch::look(&cfg, "").unwrap();
                    }
                    "replay" => {
                        replay(&cfg, &job).unwrap();
                    }
                    "restore" => {
                        restore(&job).unwrap();
                    }
                    _ => unreachable!(),
                }
                assert!(
                    crate::ask::load(&job).is_none(),
                    "{end}/{removed}/{recovery} revived an obsolete ask"
                );
            }
        }
    }
}
