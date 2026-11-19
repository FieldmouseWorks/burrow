//! A damaged member remains a member; only confirmed retirement frees it.
use super::*;

fn damage(job: &Job, name: &str, kind: &str) -> Vec<u8> {
    let path = host_path(job, name);
    let before = fs::read(&path).unwrap();
    match kind {
        "parse" => fs::write(path, "{broken").unwrap(),
        "read" => {
            fs::remove_file(&path).unwrap();
            fs::create_dir(path).unwrap();
        }
        "large" => fs::write(path, "x".repeat(65_000)).unwrap(),
        _ => unreachable!(),
    }
    before
}

fn repair(job: &Job, name: &str, before: Vec<u8>) {
    let path = host_path(job, name);
    if path.is_dir() {
        fs::remove_dir(&path).unwrap();
    }
    fs::write(path, before).unwrap();
}

#[test]
fn a_sole_unreadable_member_cannot_be_replaced_by_a_reversible_join() {
    for released in [false, true] {
        for kind in ["parse", "read", "large"] {
            let (_dir, cfg) = fixture();
            asks(&cfg, "leader", Some("subject"), true);
            let leader = Job::new(&cfg, "leader");
            if released {
                super::authority::edit_group(&cfg, "subject", |g| {
                    g["due"] = (crate::now() - 1).into();
                });
                expire(&cfg, &leader).unwrap();
            }
            let expected = open(&leader).unwrap().unwrap();
            let name = if released {
                "question-released"
            } else {
                "question"
            };
            let before = damage(&leader, name, kind);
            let registry = control_root(&cfg).join(".about/state.json");
            let saved = fs::read(&registry).unwrap();
            asks(&cfg, "new", None, false);
            let new = Job::new(&cfg, "new");
            record(
                &new,
                &Recommendation {
                    default: "take the reversible default".into(),
                    one_way: false,
                    about: Some("subject".into()),
                },
            )
            .unwrap();
            assert!(finish(&cfg, &new, "ask").is_err(), "{released} {kind}");
            assert_eq!(fs::read(&registry).unwrap(), saved);
            repair(&leader, name, before);
            finish(&cfg, &new, "ask").unwrap();
            assert_eq!(follows(&new).unwrap().as_deref(), Some("leader"));
            assert_eq!(key_of(&leader).unwrap().as_deref(), Some("subject"));
            for job in [&leader, &new] {
                let q = open(job).unwrap().unwrap();
                assert!(q.recommendation.one_way);
                assert_eq!(q.due, expected.due);
            }
        }
    }
}

#[test]
fn unknown_member_liveness_blocks_changes_and_reply_cohorts_until_repaired() {
    for released in [false, true] {
        for kind in ["parse", "read", "large"] {
            for damaged in ["leader", "follower"] {
                let (_dir, cfg) = fixture();
                asks(&cfg, "leader", Some("subject"), true);
                asks(&cfg, "follower", Some("subject"), false);
                let job = Job::new(&cfg, damaged);
                if released {
                    let mut state: serde_json::Value = serde_json::from_str(
                        &fs::read_to_string(control_root(&cfg).join(".about/state.json")).unwrap(),
                    )
                    .unwrap();
                    state["subject"]["due"] = (crate::now() - 1).into();
                    fs::write(
                        control_root(&cfg).join(".about/state.json"),
                        state.to_string(),
                    )
                    .unwrap();
                    expire(&cfg, &job).unwrap();
                }
                let name = if released {
                    "question-released"
                } else {
                    "question"
                };
                let before = damage(&job, name, kind);
                let registry = control_root(&cfg).join(".about/state.json");
                let saved = fs::read(&registry).unwrap();
                let leader = Job::new(&cfg, "leader");
                let follower = Job::new(&cfg, "follower");
                assert!(follows(&follower).is_err(), "{released} {kind} {damaged}");
                assert!(cohort(&leader).is_err(), "{released} {kind} {damaged}");
                assert!(crate::cmd::reply_answer(&cfg, "leader", "yes", false).is_err());
                assert!(merge(&cfg, "elsewhere", &["follower".into()]).is_err());
                asks(&cfg, "outside", None, false);
                assert!(merge(&cfg, "subject", &["outside".into()]).is_err());
                record(
                    &Job::new(&cfg, "outside"),
                    &Recommendation {
                        default: "leave it".into(),
                        one_way: false,
                        about: Some("subject".into()),
                    },
                )
                .unwrap();
                assert!(finish(&cfg, &Job::new(&cfg, "outside"), "ask").is_err());
                assert_eq!(fs::read(&registry).unwrap(), saved);
                repair(&job, name, before);
                assert_eq!(key_of(&job).unwrap().as_deref(), Some("subject"));
                assert!(open(&job).unwrap().unwrap().recommendation.one_way);
                assert_eq!(cohort(&leader).unwrap().len(), 1);
                assert_eq!(
                    leader.turns().as_deref(),
                    Some(if released && damaged == "leader" {
                        "2"
                    } else {
                        "1"
                    })
                );
            }
        }
    }
}
