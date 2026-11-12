use super::*;

fn deadline(cfg: &Config, id: &str, due: i64) {
    let job = Job::new(cfg, id);
    let mut q = load_raw(&job).unwrap();
    q.due = due;
    fs::write(
        host_path(&job, "question"),
        serde_json::to_string(&q).unwrap(),
    )
    .unwrap();
}

fn grouped(cfg: &Config) -> i64 {
    asks(cfg, "a", None, false);
    asks(cfg, "z", None, true);
    let due = crate::now() + 1234;
    deadline(cfg, "a", due + 700);
    deadline(cfg, "z", due);
    for (id, since) in [("a", "100"), ("z", "200")] {
        fs::write(host_path(&Job::new(cfg, id), "question-since"), since).unwrap();
    }
    assert_eq!(
        merge(cfg, "source", &["a".into(), "z".into()]).unwrap(),
        "z"
    );
    due
}

fn policy(cfg: &Config, id: &str, due: i64) {
    let q = load(&Job::new(cfg, id)).unwrap();
    assert!(
        q.recommendation.one_way,
        "{id} lost inherited one-way policy"
    );
    assert_eq!(q.due, due, "{id} changed its effective deadline");
}

fn redeclare(cfg: &Config, id: &str, key: Option<&str>) {
    let job = Job::new(cfg, id);
    record(
        &job,
        &Recommendation {
            default: "leave it".into(),
            one_way: false,
            about: key.map(str::to_owned),
        },
    )
    .unwrap();
    finish(cfg, &job, "ask").unwrap();
}

#[test]
fn inherited_policy_survives_subset_full_repeated_merges_and_rekeying() {
    for full in [false, true] {
        let (_dir, cfg) = fixture();
        let due = grouped(&cfg);
        let ids = if full {
            vec!["a".into(), "z".into()]
        } else {
            vec!["a".into()]
        };
        for key in ["destination", "destination", "third"] {
            merge(&cfg, key, &ids).unwrap();
            policy(&cfg, "a", due);
            policy(&cfg, "z", due);
        }
        redeclare(&cfg, "a", Some("fourth"));
        policy(&cfg, "a", due);
    }
}

#[test]
fn the_last_member_keeps_its_shared_deadline_across_updates_and_remerges() {
    let (_dir, cfg) = fixture();
    let due = grouped(&cfg);
    crate::cmd::stop(&cfg, "z", false).unwrap();
    for key in [None, Some("source"), Some("next")] {
        redeclare(&cfg, "a", key);
        policy(&cfg, "a", due);
        merge(&cfg, key.unwrap_or("source"), &["a".into()]).unwrap();
        policy(&cfg, "a", due);
    }
}

#[test]
fn retired_groups_impose_neither_a_stale_deadline_nor_a_one_way_requirement() {
    let (_dir, cfg) = fixture();
    grouped(&cfg);
    for id in ["a", "z"] {
        crate::cmd::stop(&cfg, id, false).unwrap();
    }
    asks(&cfg, "new", None, false);
    let due = crate::now() + 9876;
    deadline(&cfg, "new", due);
    merge(&cfg, "source", &["new".into()]).unwrap();
    let q = load(&Job::new(&cfg, "new")).unwrap();
    assert!(!q.recommendation.one_way);
    assert_eq!(q.due, due);
    crate::cmd::stop(&cfg, "new", false).unwrap();
    asks(&cfg, "fresh", Some("source"), false);
    assert!(
        !load(&Job::new(&cfg, "fresh"))
            .unwrap()
            .recommendation
            .one_way
    );
}

#[test]
fn failed_registry_writes_leave_every_source_and_destination_bound_to_old_policy() {
    let (dir, cfg) = fixture();
    let due = grouped(&cfg);
    asks(&cfg, "other", Some("occupied"), false);
    let other_due = load(&Job::new(&cfg, "other")).unwrap().due;
    let state = control_root(&cfg).join(".about/state.json");
    let before = fs::read(&state).unwrap();
    let blocked = state.with_file_name("state.json.new");
    fs::create_dir(&blocked).unwrap();
    assert!(merge(&cfg, "occupied", &["a".into(), "z".into()]).is_err());
    assert_eq!(fs::read(&state).unwrap(), before);
    policy(&cfg, "a", due);
    policy(&cfg, "z", due);
    assert_eq!(key_of(&Job::new(&cfg, "a")).as_deref(), Some("source"));
    assert_eq!(load(&Job::new(&cfg, "other")).unwrap().due, other_due);
    fs::remove_dir(blocked).unwrap();
    // A truncated, interrupted preparation is not authority either.
    fs::write(state.with_file_name("state.json.new"), "{partial").unwrap();
    merge(&cfg, "occupied", &["a".into()]).unwrap();
    assert!(load(&Job::new(&cfg, "a")).unwrap().recommendation.one_way);
    assert_eq!(load(&Job::new(&cfg, "a")).unwrap().due, other_due);
    assert!(dir.path().exists());
}

#[test]
fn damaged_authority_cannot_execute_or_redeclare_a_weaker_default() {
    let (_dir, cfg) = fixture();
    grouped(&cfg);
    fs::write(control_root(&cfg).join(".about/state.json"), "{damaged").unwrap();
    let job = Job::new(&cfg, "a");
    assert!(load(&job).is_none());
    assert!(expire(&cfg, &job).unwrap().is_none());
    assert_eq!(job.turns().as_deref(), Some("1"));
    record(
        &job,
        &Recommendation {
            default: "leave it".into(),
            one_way: false,
            about: Some("new".into()),
        },
    )
    .unwrap();
    assert!(finish(&cfg, &job, "ask").is_err());
    assert!(merge(&cfg, "new", &["a".into()]).is_err());
}

#[test]
fn merges_release_all_locks_against_replies_expiry_and_triage_in_either_order() {
    for action in ["reply", "expiry", "admission"] {
        for busy in ["a", "z"] {
            let (dir, mut cfg) = fixture();
            let inbox = cfg.inbox.as_mut().unwrap();
            inbox.state = dir.path().join("inbox");
            inbox.wake = dir.path().join("inbox-wake");
            inbox.triage = Some(crate::config::Triage {
                sort_unlabelled: true,
                owner_label: "owner".into(),
                owner: None,
                skip: vec![],
                at_once: 4,
                drain: None,
                drain_jobs: 4,
            });
            for id in ["a", "m", "z"] {
                asks(&cfg, id, None, false);
                deadline(&cfg, id, crate::now() - 1);
                let record = cfg.inbox.as_ref().unwrap().state.join(".triage").join(id);
                fs::create_dir_all(&record).unwrap();
                fs::write(record.join("job"), id).unwrap();
            }
            merge(&cfg, "subject", &["a".into(), "m".into(), "z".into()]).unwrap();
            let job = Job::new(&cfg, busy);
            let held = job.lock().unwrap();
            // A reply/timeout owns busy while admission inspects the others.
            // Merge must finish without requiring that owner to release it.
            std::thread::scope(|scope| {
                let (tx, rx) = std::sync::mpsc::channel();
                let cfg = &cfg;
                scope.spawn(move || {
                    tx.send(merge(cfg, "next", &["z".into(), "m".into(), "a".into()]))
                        .unwrap();
                });
                let result = rx.recv_timeout(std::time::Duration::from_secs(2));
                if let Ok(result) = result {
                    assert!(result.is_err());
                    // These are the actual paths that scan other job locks.
                    match action {
                        "reply" => crate::turn::start_answer(cfg, busy, "yes", false).unwrap(),
                        "expiry" => {
                            expire(cfg, &job).unwrap();
                        }
                        "admission" => assert!(matches!(
                            crate::inbox::resumption::admission(cfg, &job).unwrap(),
                            crate::inbox::resumption::Resume::Ready(_)
                        )),
                        _ => unreachable!(),
                    }
                    drop(held);
                } else {
                    // Let a broken blocking implementation finish before failing.
                    drop(held);
                    panic!("merge blocked an {action} holding {busy}");
                }
            });
            for id in ["a", "m", "z"] {
                assert!(Job::new(&cfg, id).try_lock().unwrap().is_some());
            }
        }
    }
}

#[test]
fn migration_accepts_live_released_and_legacy_owner_questions_and_promotes_them() {
    for legacy in [false, true] {
        let (dir, cfg) = fixture();
        for id in ["released", "live"] {
            asks(&cfg, id, None, id == "released");
        }
        let due = crate::now() - 1;
        deadline(&cfg, "released", due);
        let released = Job::new(&cfg, "released");
        {
            let _held = released.lock().unwrap();
            expire(&cfg, &released).unwrap();
        }
        assert!(load(&released).is_none());
        assert!(host_path(&released, "question-needs-owner").exists());
        if legacy {
            fs::remove_file(host_path(&released, "question-released")).unwrap();
        }
        for (id, since) in [("released", "100"), ("live", "200")] {
            fs::write(host_path(&Job::new(&cfg, id), "question-since"), since).unwrap();
        }
        notified(&cfg, &["released", "live"]);
        assert_eq!(
            merge(&cfg, "subject", &["released".into(), "live".into()]).unwrap(),
            "live"
        );
        assert!(
            load(&Job::new(&cfg, "live"))
                .unwrap()
                .recommendation
                .one_way
        );
        notified(&cfg, &["released", "live"]);
        assert!(calls(&dir).contains("close|proj-question-released-1"));
        // Re-merge just the released member and stop its new leader.
        merge(&cfg, "next", &["released".into()]).unwrap();
        notified(&cfg, &["released"]);
        assert!(calls(&dir).contains("ensure|proj-question-released-1"));
        assert!(follows(&released).is_none());
        assert!(expire(&cfg, &released).unwrap().is_none());
        assert_eq!(
            released.turns().as_deref(),
            Some("2"),
            "a released default never launches"
        );
        assert!(!released.path("prompt.3").exists());
        crate::cmd::reply_answer(&cfg, "released", "leave it", false).unwrap();
        assert_eq!(released.turns().as_deref(), Some("3"));
        assert!(!host_path(&released, "question-released").exists());
    }
}

#[test]
fn partially_published_replacement_ids_cannot_join_an_old_answer_cohort() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "a", Some("subject"), false);
    asks(&cfg, "z", Some("subject"), false);
    let leader = Job::new(&cfg, "a");
    let follower = Job::new(&cfg, "z");
    let original = cohort(&leader)[0].1.clone();
    // A replacement ID has been allocated, but its question is not published.
    fs::write(host_path(&follower, "question-id"), "replacement").unwrap();
    assert_eq!(cohort(&leader)[0].1, original);
    // A replacement question published before its binding commits must not
    // be mistaken for the registered question either.
    let mut raw: serde_json::Value =
        serde_json::from_str(&read(&host_path(&follower, "question")).unwrap()).unwrap();
    raw["id"] = serde_json::json!("replacement");
    fs::write(host_path(&follower, "question"), raw.to_string()).unwrap();
    assert!(cohort(&leader).is_empty());
    assert!(!still_waiting(&follower, "subject", &original));
}

#[test]
fn a_released_question_keeps_its_original_deadline_and_policy_when_it_alone_moves() {
    for legacy in [false, true] {
        let (_dir, mut cfg) = fixture();
        asks(&cfg, "released", None, true);
        let due = crate::now() - 10;
        deadline(&cfg, "released", due);
        let released = Job::new(&cfg, "released");
        expire(&cfg, &released).unwrap();
        if legacy {
            fs::remove_file(host_path(&released, "question-released")).unwrap();
        }
        // A config update cannot change the deadline of a released question.
        cfg.ask_timeout += 999;
        merge(&cfg, "subject", &["released".into()]).unwrap();
        asks(&cfg, "follower", Some("subject"), false);
        policy(&cfg, "follower", due);
        merge(&cfg, "next", &["released".into()]).unwrap();
        asks(&cfg, "another", Some("next"), false);
        policy(&cfg, "another", due);
    }
}

#[test]
fn upgrading_legacy_registry_members_captures_the_publication_not_a_preallocated_id() {
    let (_dir, cfg) = fixture();
    for id in ["a", "z"] {
        asks(&cfg, id, None, false);
        let job = Job::new(&cfg, id);
        let mut raw: serde_json::Value =
            serde_json::from_str(&read(&host_path(&job, "question")).unwrap()).unwrap();
        raw.as_object_mut().unwrap().remove("id");
        raw["recommendation"]["about"] = serde_json::json!("legacy");
        fs::write(host_path(&job, "question"), raw.to_string()).unwrap();
        fs::write(host_path(&job, "question-about"), "legacy").unwrap();
    }
    let root = control_root(&cfg).join(".about");
    fs::create_dir_all(&root).unwrap();
    let due = crate::now() + 321;
    fs::write(
        root.join("6c6567616379"),
        serde_json::json!({"due":due,"one_way":true,"members":["a","z"]}).to_string(),
    )
    .unwrap();
    let leader = Job::new(&cfg, "a");
    let original = cohort(&leader)[0].1.clone();
    fs::write(
        host_path(&Job::new(&cfg, "z"), "question-id"),
        "unpublished replacement",
    )
    .unwrap();
    assert_eq!(cohort(&leader)[0].1, original);
    merge(&cfg, "next", &["z".into()]).unwrap();
    policy(&cfg, "z", due);
    // Upgrade a legacy publication in place, preserving order and deadline.
    redeclare(&cfg, "a", None);
    policy(&cfg, "a", due);
    assert!(follows(&leader).is_none());
}

#[test]
fn interrupted_rekey_publication_keeps_binding_and_policy_together() {
    let (_dir, cfg) = fixture();
    let due = grouped(&cfg);
    let job = Job::new(&cfg, "a");
    let mut q = load(&job).unwrap();
    q.recommendation.about = Some("new subject".into());
    assert!(
        about::publish(&job, "new subject", q, |_| Err(
            "publication interrupted".into()
        ))
        .is_err()
    );
    assert_eq!(key_of(&job).as_deref(), Some("new subject"));
    policy(&cfg, "a", due);
    policy(&cfg, "z", due);
    redeclare(&cfg, "a", None);
    policy(&cfg, "a", due);
    assert_eq!(key_of(&job).as_deref(), Some("new subject"));
}

#[test]
fn writable_stop_markers_do_not_remove_host_owned_shared_question_authority() {
    let (_dir, cfg) = fixture();
    let due = grouped(&cfg);
    let job = Job::new(&cfg, "a");
    fs::write(job.turn("1", "stopped"), "guest marker").unwrap();
    policy(&cfg, "a", due);
    assert_eq!(key_of(&job).as_deref(), Some("source"));
    assert!(follows(&job).is_some());
}
