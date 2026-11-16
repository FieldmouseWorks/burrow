use super::tests::fixture;
use super::*;
use crate::engine::Engine;

fn pending(job: &Job, due: u64, targets: Vec<String>) {
    crate::state_reads::write(
        job.path(PENDING),
        serde_json::to_vec(&Pending {
            targets,
            retry_at: due,
            turn: job.turns().unwrap(),
            baselines: String::new(),
        })
        .unwrap(),
    )
    .unwrap();
    park(job, due as i64).unwrap();
    crate::state_reads::write(job.turn("1", "outcome"), "GitHub quota; waiting for reset").unwrap();
}
fn due(job: &Job) {
    if let Some(text) = read(&job.path(PENDING))
        && let Ok(mut p) = serde_json::from_str::<Pending>(&text)
    {
        p.retry_at = 1;
        crate::state_reads::write(job.path(PENDING), serde_json::to_vec(&p).unwrap()).unwrap();
    }
    for entry in fs::read_dir(job.turn(&job.turns().unwrap(), "github-refusals"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
    {
        if entry.path().extension().is_some_and(|s| s == "json")
            && let Some(mut m) = Meter::read(&entry.path())
        {
            m.retry_at = Some(1);
            fs::write(entry.path(), serde_json::to_vec(&m).unwrap()).unwrap();
        }
    }
    park(job, 1).unwrap();
}

fn job(cfg: &Config) -> Job<'_> {
    let job = Job::new(cfg, "recovery");
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

#[test]
fn quota_preserves_owner_questions_through_finalization_and_restart() {
    for committed in [false, true] {
        let (_d, cfg) = fixture();
        let job = job(&cfg);
        fs::write(job.turn("1", "result"), "May I erase the remote archive?").unwrap();
        crate::ask::record(
            &job,
            &crate::ask::Recommendation {
                default: "Keep the archive until the owner answers".into(),
                one_way: true,
                about: None,
            },
        )
        .unwrap();
        fs::write(job.path("end.next"), "ask").unwrap();
        if committed {
            crate::ask::finish(&cfg, &job, "ask").unwrap();
        }
        let before: Vec<_> = ["question", "question-id", "question-since", "question-text"]
            .map(|name| (name, read(&crate::ask::host_path(&job, name))))
            .into_iter()
            .collect();
        pending(&job, 1, vec!["sam/r#7".into()]);
        if committed {
            fs::write(job.path("await"), format!("sam/r#7\t{}\t{{}}\n", now())).unwrap();
        }
        assert!(finish(&job, "1").unwrap());
        assert!(!job.path("await").exists());
        let q = crate::ask::load(&job)
            .unwrap()
            .expect("quota finalization lost the owner question");
        assert!(q.recommendation.one_way);
        assert_eq!(
            q.recommendation.default,
            "Keep the archive until the owner answers"
        );
        assert_eq!(
            read(&crate::ask::host_path(&job, "question-text")).as_deref(),
            Some("May I erase the remote archive?")
        );
        if committed {
            for (name, value) in before {
                assert_eq!(read(&crate::ask::host_path(&job, name)), value);
            }
        }
        // Repeat host finalization, then reconstruct the watcher after reset.
        assert!(finish(&job, "1").unwrap());
        let id = read(&crate::ask::host_path(&job, "question-id"));
        crate::watch::look(&cfg, "").unwrap();
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert_eq!(read(&crate::ask::host_path(&job, "question-id")), id);
        assert_eq!(crate::ask::load(&job).unwrap().unwrap().due, q.due);
        assert_eq!(read(&job.turn("1", "end")).as_deref(), Some("ask"));
        // Direct recovery entry points cannot publish the old await over it.
        replay(&cfg, &job).unwrap();
        restore(&job).unwrap();
        assert_eq!(read(&crate::ask::host_path(&job, "question-id")), id);
        assert_eq!(crate::ask::load(&job).unwrap().unwrap().due, q.due);
        // Expiry retains the owner hold; another quota park cannot release it.
        let mut expired = crate::ask::load(&job).unwrap().unwrap();
        expired.due = now() - 1;
        fs::write(
            crate::ask::host_path(&job, "question"),
            serde_json::to_vec(&expired).unwrap(),
        )
        .unwrap();
        crate::ask::expire(&cfg, &job).unwrap();
        let owner = crate::ask::host_path(&job, "question-needs-owner");
        assert!(owner.exists());
        park(&job, 1).unwrap();
        crate::watch::look(&cfg, "").unwrap();
        assert!(owner.exists());
        assert!(crate::turn::start(&cfg, job.id, "quota reset", false).is_err());
    }
}

#[test]
fn quota_restart_commits_a_pending_one_way_ask_before_replaying() {
    let (_d, cfg) = fixture();
    let job = job(&cfg);
    pending(&job, 1, vec!["sam/r#7".into()]);
    crate::ask::record(
        &job,
        &crate::ask::Recommendation {
            default: "Wait for the owner".into(),
            one_way: true,
            about: None,
        },
    )
    .unwrap();
    fs::write(job.turn("1", "result"), "Which archive can be erased?").unwrap();
    fs::write(job.path("end.next"), "ask").unwrap();
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(
        crate::ask::load(&job)
            .unwrap()
            .unwrap()
            .recommendation
            .one_way
    );
    let wake: serde_json::Value = serde_json::from_str(&read(&cfg.wake).unwrap()).unwrap();
    assert_eq!(
        wake[""].as_i64(),
        Some(crate::ask::load(&job).unwrap().unwrap().due)
    );
    assert_eq!(
        read(&crate::ask::host_path(&job, "question-text")).as_deref(),
        Some("Which archive can be erased?")
    );
}

#[test]
fn ordinary_replay_errors_retire_only_after_an_accepted_resume() {
    for problem in ["denied", "invalid", "closed", "malformed"] {
        let (d, mut cfg) = fixture();
        let launcher = d.path().join("launch");
        burrow_test::executable(
            &launcher,
            format!("#!/bin/sh\n[ ! -e '{}/fail-start' ]\n", d.path().display()),
        );
        cfg.tools.systemd_run = launcher;
        let job = job(&cfg);
        let target = if problem == "invalid" {
            "bogus"
        } else {
            "sam/r#7"
        };
        pending(&job, 1, vec![target.into()]);
        due(&job);
        if problem == "malformed" {
            crate::state_reads::write(job.path(PENDING), "{malformed").unwrap();
        }
        if problem == "denied" {
            fs::write(d.path().join("denied"), "").unwrap();
        }
        if problem == "closed" {
            burrow_test::executable(
                &cfg.tools.gh,
                "#!/bin/sh\nprintf 'HTTP/2.0 200 OK\\n\\n'\ncase \"$*\" in */comments*) echo '[]';; *) echo '{\"state\":\"closed\"}';; esac\n",
            );
        }
        fs::write(d.path().join("fail-start"), "").unwrap();
        crate::watch::look(&cfg, "").unwrap();
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert!(
            job.path(PENDING).exists(),
            "lost intent on failed start: {problem}"
        );
        fs::remove_file(d.path().join("fail-start")).unwrap();
        crate::watch::look(&cfg, "").unwrap();
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert!(!job.path(PENDING).exists(), "stale intent after {problem}");
        assert!(!finish(&job, "2").unwrap());
        let recommendation = crate::ask::Recommendation {
            default: "A real owner decision".into(),
            one_way: false,
            about: None,
        };
        crate::ask::record(&job, &recommendation).unwrap();
        crate::ask::finish(&cfg, &job, "ask").unwrap();
        assert!(crate::ask::load(&job).unwrap().is_some());
    }
}

#[test]
fn saved_baselines_survive_later_direct_and_await_refusals() {
    for another_await in [false, true] {
        let (d, cfg) = fixture();
        let job = job(&cfg);
        crate::cmd::record_wait(
            &cfg,
            &job,
            &[("sam/r".into(), "7".into())],
            &["sam/r#7".into()],
            None,
        )
        .unwrap();
        let original = read(&job.path("await.next")).unwrap();
        if another_await {
            fs::write(d.path().join("limited"), "").unwrap();
            crate::cmd::record_wait(
                &cfg,
                &job,
                &[("sam/r".into(), "8".into())],
                &["sam/r#8".into()],
                None,
            )
            .unwrap();
            fs::remove_file(d.path().join("limited")).unwrap();
        } else {
            let mut m = Meter::new("workers", "sam", now() as u64);
            m.retry_at = Some((now() + 3600) as u64);
            m.record(&job.turn("1", "github-refusals")).unwrap();
            // ask/done clears the active declaration, but quota recovery
            // must retain the earlier successful baseline.
            fs::remove_file(job.path("await.next")).unwrap();
            fs::write(job.path("end.next"), "ask").unwrap();
        }
        assert!(finish(&job, "1").unwrap());
        fs::write(d.path().join("changed"), "").unwrap();
        due(&job);
        crate::watch::look(&cfg, "").unwrap();
        let restored = read(&job.path("await")).unwrap();
        assert!(restored.starts_with(&original));
        valid_records(
            &restored,
            if another_await {
                &["sam/r#7", "sam/r#8"]
            } else {
                &["sam/r#7"]
            },
        );
        if another_await {
            assert!(restored.contains("sam/r#8\t"));
        }
        assert_eq!(job.turns().as_deref(), Some("1"));
        crate::watch::look(&cfg, "").unwrap();
        assert_eq!(
            job.turns().as_deref(),
            Some("2"),
            "another={another_await}; original={original}; restored={restored}; current={:?}",
            read(&job.path("await"))
        );
    }
}

#[test]
fn published_waits_win_over_intent_left_by_a_crash() {
    let (_d, cfg) = fixture();
    let job = job(&cfg);
    pending(&job, 1, vec!["github-limit/r#7".into()]);
    let intent = read(&job.path(PENDING)).unwrap();
    replay(&cfg, &job).unwrap();
    let registered = read(&job.path("await")).unwrap();
    crate::state_reads::write(job.path(PENDING), intent).unwrap(); // crash before retiring intent
    assert!(!finish(&job, "1").unwrap());
    assert_eq!(read(&job.path("await")).unwrap(), registered);
    assert!(!job.path(PENDING).exists());
}

#[test]
fn per_installation_holds_and_job_maxima_are_independent() {
    let (_d, cfg) = fixture();
    let job = job(&cfg);
    pending(&job, 2000, vec!["sam/r#7".into()]);
    for (app, owner, due) in [("workers", "sam", 6000), ("other", "eve", 3000)] {
        let mut m = Meter::new(app, owner, 1000);
        m.retry_at = Some(due);
        m.record(&job.turn("1", "github-refusals")).unwrap();
    }
    assert_eq!(refused(&job), Some(6000));
    finish(&job, "1").unwrap();
    assert!(read(&job.path("await")).unwrap().contains("\t6000"));
    // A later turn cannot inherit earlier command refusals, even after a
    // crash between accepted start and deleting old intent.
    job.set_turns(2).unwrap();
    fs::remove_file(job.path("await")).unwrap();
    assert!(!finish(&job, "2").unwrap());
}

#[test]
fn host_registration_and_replay_attribute_the_configured_client() {
    for selected in [None, Some("job-app")] {
        let (d, mut cfg) = fixture();
        cfg.host_github_app = Some("host-app".into());
        let job = job(&cfg);
        if let Some(app) = selected {
            crate::state_reads::write(job.path("app"), app).unwrap();
        }
        crate::state_reads::write(job.turn("1", "engine"), Engine::Codex.name()).unwrap();
        crate::state_reads::write(job.path("link"), "sam/r#7").unwrap();
        fs::write(d.path().join("limited"), "").unwrap();
        crate::cmd::record_wait(
            &cfg,
            &job,
            &[("sam/r".into(), "7".into())],
            &["sam/r#7".into()],
            None,
        )
        .unwrap();
        let initial = meters(&cfg);
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].app, "host-app");
        // Shorten only the stored fixture evidence to prove replay renews
        // the host's installation, not the engine's selected App.
        for e in fs::read_dir(cfg.github_meters.as_ref().unwrap())
            .unwrap()
            .filter_map(Result::ok)
        {
            if e.path().extension().is_some_and(|s| s == "json") {
                fs::remove_file(e.path()).unwrap();
            }
        }
        pending(&job, 1, vec!["sam/r#7".into()]);
        let due = replay(&cfg, &job).unwrap().unwrap();
        let renewed = meters(&cfg);
        assert_eq!(renewed[0].app, "host-app");
        assert_eq!(renewed[0].retry_at, Some(due as u64));
        assert!(held(&cfg, &job, Engine::Codex).is_none());
    }
}

#[test]
fn observation_and_publication_failures_keep_the_intent_without_an_engine() {
    let (d, mut cfg) = fixture();
    cfg.host_github_app = Some("host".into());
    let job = job(&cfg);
    fs::write(job.path("github"), "blocked meter directory").unwrap();
    fs::write(d.path().join("limited"), "").unwrap();
    crate::cmd::record_wait(
        &cfg,
        &job,
        &[("sam/r".into(), "7".into())],
        &["sam/r#7".into()],
        None,
    )
    .unwrap();
    assert!(finish(&job, "1").unwrap());
    // A second quota refusal still parks even when only meter storage fails.
    due(&job);
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(job.path(PENDING).exists());
    fs::remove_file(d.path().join("limited")).unwrap();
    fs::create_dir(job.turn("1", "outcome")).unwrap_err(); // existing completed declaration
    fs::remove_file(job.turn("1", "outcome")).unwrap();
    fs::create_dir(job.turn("1", "outcome")).unwrap();
    crate::state_reads::write(job.turn("1", "end"), "await").unwrap();
    due(&job);
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(job.path(PENDING).exists());
    assert!(read(&job.path("await")).unwrap().starts_with(LIMIT));
    let captured = read(&job.path("await.next")).unwrap();
    fs::remove_dir(job.turn("1", "outcome")).unwrap();
    // Recovery publishes the checkpoint, never asks GitHub to retake it.
    fs::write(d.path().join("denied"), "").unwrap();
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(read(&job.path("await")).unwrap(), captured);
    assert!(!job.path(PENDING).exists());
    assert_eq!(job.turns().as_deref(), Some("1"));
}

#[test]
fn a_multi_target_quota_refusal_retains_the_successfully_captured_prefix() {
    let (d, cfg) = fixture();
    let job = job(&cfg);
    burrow_test::executable(
        &cfg.tools.gh,
        format!(
            r#"#!/bin/sh
case "$*" in
 */issues/8*) if [ ! -e '{0}/reset' ]; then printf 'HTTP/2.0 403 Forbidden\nretry-after: 1234\n\n{{}}'; exit 1; fi;;
esac
printf 'HTTP/2.0 200 OK\n\n'
case "$*" in */comments*) if [ -e '{0}/reset' ]; then echo '[{{"body":"news"}}]'; else echo '[]'; fi;; *) echo '{{"state":"open"}}';; esac
"#,
            d.path().display()
        ),
    );
    crate::cmd::record_wait(
        &cfg,
        &job,
        &[("sam/r".into(), "7".into()), ("sam/r".into(), "8".into())],
        &["sam/r#7".into(), "sam/r#8".into()],
        None,
    )
    .unwrap();
    let original = captured(&job).unwrap();
    assert!(original.starts_with("sam/r#7\t"));
    assert!(!original.contains("sam/r#8"));
    finish(&job, "1").unwrap();
    due(&job);
    fs::write(d.path().join("reset"), "").unwrap();
    crate::watch::look(&cfg, "").unwrap();
    assert!(read(&job.path("await")).unwrap().starts_with(&original));
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(job.turns().as_deref(), Some("2"));
}

fn valid_records(text: &str, targets: &[&str]) {
    let rows = crate::job::awaits(text);
    assert_eq!(rows.len(), targets.len(), "{text}");
    for (row, target) in rows.iter().zip(targets) {
        assert_eq!(&row.target, target);
        assert!(row.since.parse::<i64>().is_ok());
        assert!(
            serde_json::from_str::<serde_json::Value>(&row.was)
                .unwrap()
                .is_object()
        );
    }
}
#[test]
fn restored_baselines_have_independent_records_and_keep_the_first_capture() {
    let (_d, cfg) = fixture();
    let job = job(&cfg);
    remember(&job, "sam/a#1\t10\t{\"state\":\"open\"}").unwrap();
    remember(
        &job,
        "sam/a#1\t20\t{\"state\":\"closed\"}\nsam/b#2\t30\t{}\n",
    )
    .unwrap();
    restore(&job).unwrap();
    let text = read(&job.path("await")).unwrap();
    valid_records(&text, &["sam/a#1", "sam/b#2"]);
    assert_eq!(crate::job::awaits(&text)[0].since, "10");
}
#[test]
fn committed_intent_is_recovered_without_a_marker_and_never_before_its_deadline() {
    let (d, cfg) = fixture();
    let job = job(&cfg);
    let reset = now() + 3600;
    pending(&job, reset as u64, vec!["sam/r#7".into()]);
    fs::remove_file(job.path("await")).unwrap(); // crash before finalization
    burrow_test::executable(
        &cfg.tools.gh,
        format!("#!/bin/sh\ntouch '{}/called'\nexit 1\n", d.path().display()),
    );
    crate::watch::look(&cfg, "").unwrap();
    assert!(
        read(&job.path("await"))
            .unwrap()
            .ends_with(&reset.to_string())
    );
    assert!(!d.path().join("called").exists());
    assert_eq!(job.turns().as_deref(), Some("1"));
    // A repeated refusal committed a later reset before replacing its marker.
    pending(&job, (reset + 3600) as u64, vec!["sam/r#7".into()]);
    crate::state_reads::write(job.path("await"), format!("{LIMIT}\t1\t1\n")).unwrap();
    assert_eq!(job.facts().unwrap().quota_reset.unwrap().1, reset + 3600);
    crate::watch::look(&cfg, "").unwrap();
    assert!(
        read(&job.path("await"))
            .unwrap()
            .ends_with(&(reset + 3600).to_string())
    );
    let wake: serde_json::Value = serde_json::from_str(&read(&cfg.wake).unwrap()).unwrap();
    assert_eq!(wake[""], reset + 3600);
    assert!(!d.path().join("called").exists());
}
#[test]
fn running_and_stopped_jobs_are_not_revived_from_intent() {
    for running in [true, false] {
        let (_d, mut cfg) = fixture();
        if running {
            cfg.tools.systemctl = "true".into();
        }
        let job = job(&cfg);
        pending(&job, (now() + 3600) as u64, vec!["sam/r#7".into()]);
        fs::remove_file(job.path("await")).unwrap();
        if running {
            crate::state_reads::write(job.turn("1", "slot"), "1").unwrap();
        } else {
            crate::state_reads::write(job.turn("1", "stopped"), "").unwrap();
        }
        crate::watch::look(&cfg, "").unwrap();
        assert!(!job.path("await").exists());
        assert_eq!(job.turns().as_deref(), Some("1"));
        let wake: serde_json::Value = serde_json::from_str(&read(&cfg.wake).unwrap()).unwrap();
        if running {
            let p: Pending = serde_json::from_str(&read(&job.path(PENDING)).unwrap()).unwrap();
            assert_eq!(wake[""], p.retry_at);
        } else {
            assert!(wake[""].is_null());
        }
    }
}
#[test]
fn queued_watchdog_records_follow_installation_holds_and_release_grace() {
    let (d, mut cfg) = fixture();
    cfg.tools.systemctl = "true".into();
    cfg.stall_after_minutes = 30;
    cfg.waits = Some(d.path().join("watchdog"));
    let since = now() - 10;
    for (id, engine, app, emergency) in [
        ("claude", Engine::Claude, "workers", false),
        ("codex", Engine::Codex, "workers", false),
        ("emergency", Engine::Claude, "workers", true),
        ("other", Engine::Codex, "other", false),
    ] {
        let job = Job::new(&cfg, id);
        fs::create_dir_all(&job.dir).unwrap();
        crate::state_reads::write(job.path("app"), app).unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Automated,
        )
        .unwrap();
        job.set_turns(1).unwrap();
        crate::state_reads::write(job.path("link"), "sam/r#7").unwrap();
        crate::state_reads::write(job.turn("1", "engine"), engine.name()).unwrap();
        crate::state_reads::write(job.path("prompt.1"), "queued").unwrap();
        fs::File::open(job.path("prompt.1"))
            .unwrap()
            .set_times(
                std::fs::FileTimes::new().set_modified(
                    std::time::UNIX_EPOCH + std::time::Duration::from_secs(since as u64),
                ),
            )
            .unwrap();
        if emergency {
            crate::state_reads::write(job.path("unstick"), "").unwrap();
        }
        crate::waits::rewatch(&cfg, &[id]);
    }
    let observer = cfg.jobs.join("observation");
    fs::create_dir_all(&observer).unwrap();
    let mut m = Meter::new("workers", "sam", now() as u64);
    m.retry_at = Some((now() + 3600) as u64);
    m.record(cfg.github_meters.as_ref().unwrap()).unwrap();
    for deadline in [now() + 3600, now() + 7200, now() - 1] {
        m.retry_at = Some(deadline as u64);
        let file = fs::read_dir(cfg.github_meters.as_ref().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .find(|e| e.path().extension().is_some_and(|s| s == "json"))
            .unwrap()
            .path();
        fs::write(file, serde_json::to_vec(&m).unwrap()).unwrap();
        crate::watch::look(&cfg, "").unwrap(); // the quota-observation event
        let snapshot: serde_json::Value =
            serde_json::from_str(&read(cfg.waits.as_ref().unwrap()).unwrap()).unwrap();
        for id in ["claude", "codex", "emergency"] {
            let fact = Job::new(&cfg, id).facts().unwrap();
            assert_eq!(fact.quota_reset, Some((since, deadline)));
            assert_eq!(
                snapshot["items"][id]["due"].as_f64(),
                Some((deadline + cfg.stall_queued()) as f64)
            );
        }
        assert!(
            Job::new(&cfg, "other")
                .facts()
                .unwrap()
                .quota_reset
                .is_none()
        );
        assert_eq!(
            snapshot["items"]["other"]["due"].as_f64(),
            Some((since + cfg.stall_queued()) as f64)
        );
    }
}

#[test]
fn replay_checkpoints_each_baseline_before_a_later_target_is_limited() {
    let (d, cfg) = fixture();
    let job = job(&cfg);
    pending(&job, 1, vec!["sam/r#1".into(), "sam/r#2".into()]);
    burrow_test::executable(
        &cfg.tools.gh,
        format!(
            r#"#!/bin/sh
case "$*" in
 *issues/2*) printf 'HTTP/2.0 429 Too Many Requests\nretry-after: 3600\n\n{{}}'; exit 1;;
 *issues/1/comments*) echo called >> '{}/first-calls'; printf 'HTTP/2.0 200 OK\n\n[]';;
 *) printf 'HTTP/2.0 200 OK\n\n{{"state":"open"}}';;
esac
"#,
            d.path().display()
        ),
    );
    assert!(replay(&cfg, &job).unwrap().is_some());
    valid_records(&captured(&job).unwrap(), &["sam/r#1"]);
    // The next attempt must retain this baseline rather than hiding news
    // that arrived between the two reset windows.
    burrow_test::executable(
        &cfg.tools.gh,
        format!(
            r#"#!/bin/sh
case "$*" in
 *issues/1*) echo recaptured >> '{}/first-calls'; exit 1;;
 *comments*) printf 'HTTP/2.0 200 OK\n\n[]';;
 *) printf 'HTTP/2.0 200 OK\n\n{{"state":"open"}}';;
esac
"#,
            d.path().display()
        ),
    );
    assert_eq!(replay(&cfg, &job).unwrap(), None);
    valid_records(&read(&job.path("await")).unwrap(), &["sam/r#1", "sam/r#2"]);
    assert_eq!(
        read(&d.path().join("first-calls")).as_deref(),
        Some("called")
    );
}

#[test]
fn job_files_cannot_impose_installation_holds_or_meter_readings() {
    let (d, cfg) = fixture();
    let job = job(&cfg);
    crate::state_reads::write(job.path("app"), "workers").unwrap();
    crate::state_reads::write(job.path("link"), "sam/r#7").unwrap();
    for (app, owner) in [("workers", "sam"), ("other", "elsewhere")] {
        let mut forged = Meter::new(app, owner, now() as u64);
        forged.remaining = Some(0);
        forged.reset = Some(9999999999);
        forged.retry_at = Some(9999999999);
        fs::create_dir_all(job.path("github")).unwrap();
        forged.record(&job.path("github")).unwrap();
        crate::state_reads::write(
            job.path("github").join("unrelated-filename.json"),
            serde_json::to_vec(&forged).unwrap(),
        )
        .unwrap();
    }
    assert!(meters(&cfg).is_empty());
    assert!(held(&cfg, &job, Engine::Codex).is_none());
    let json = serde_json::to_value(crate::snapshot::Snapshot::take(&cfg, 15).unwrap()).unwrap();
    assert!(
        json["meters"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.get("installation").is_none())
    );
    // Replacing a former observation and changing labels are no authority.
    crate::state_reads::write(job.path("app"), "other").unwrap();
    assert!(held(&cfg, &job, Engine::Codex).is_none());
    let _ = d;
}

#[test]
fn initial_intent_commits_its_prefix_even_when_archive_publication_fails() {
    let (d, cfg) = fixture();
    let job = job(&cfg);
    burrow_test::executable(
        &cfg.tools.gh,
        format!(
            r#"#!/bin/sh
case "$*" in
 *issues/2*) mkdir -p '{0}/1.github-awaits'; printf 'HTTP/2.0 403 Forbidden\nretry-after: 120\n\n{{}}'; exit 1;;
 *comments*) printf 'HTTP/2.0 200 OK\n\n[]';;
 *) printf 'HTTP/2.0 200 OK\n\n{{"state":"open"}}';;
esac
"#,
            job.dir.display()
        ),
    );
    let found = [("sam/r".into(), "1".into()), ("sam/r".into(), "2".into())];
    assert!(
        crate::cmd::record_wait(
            &cfg,
            &job,
            &found,
            &["sam/r#1".into(), "sam/r#2".into()],
            None
        )
        .is_err()
    );
    let saved = read(&job.path(PENDING)).unwrap();
    let p: Pending = serde_json::from_str(&saved).unwrap();
    valid_records(&p.baselines, &["sam/r#1"]);
    due(&job);
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(job.path(PENDING).exists());
    fs::remove_dir(job.turn("1", "github-awaits")).unwrap();
    burrow_test::executable(
        &cfg.tools.gh,
        format!(
            r#"#!/bin/sh
case "$*" in
 *issues/1*) touch '{0}/recaptured'; exit 1;;
 *comments*) printf 'HTTP/2.0 200 OK\n\n[]';;
 *) printf 'HTTP/2.0 200 OK\n\n{{"state":"open"}}';;
esac
"#,
            d.path().display()
        ),
    );
    crate::watch::look(&cfg, "").unwrap();
    assert!(!d.path().join("recaptured").exists());
    valid_records(&read(&job.path("await")).unwrap(), &["sam/r#1", "sam/r#2"]);
}

#[test]
fn unreadable_intent_archive_and_publication_are_retried_without_recapture() {
    for file in [PENDING, "1.github-awaits", "await", "await.next"] {
        let (d, cfg) = fixture();
        let job = job(&cfg);
        pending(&job, 1, vec!["sam/r#7".into()]);
        due(&job);
        remember(&job, "sam/r#1\t1\t{}\n").unwrap();
        let path = job.path(file);
        let saved = state(&path).unwrap();
        if saved.is_some() {
            fs::remove_file(&path).unwrap();
        }
        fs::create_dir(&path).unwrap();
        burrow_test::executable(
            &cfg.tools.gh,
            format!(
                "#!/bin/sh\ntouch '{}/recaptured'\nexit 1\n",
                d.path().display()
            ),
        );
        crate::watch::look(&cfg, "").unwrap();
        assert!(!d.path().join("recaptured").exists(), "{file}");
        assert!(path.is_dir(), "{file}");
        assert_eq!(job.turns().as_deref(), Some("1"));
        let wake: serde_json::Value = serde_json::from_str(&read(&cfg.wake).unwrap()).unwrap();
        assert!(wake[""].as_i64().is_some(), "{file}: {wake}");
    }
}

#[test]
fn old_markers_and_interrupted_cancellation_cannot_revive_work() {
    for stopped in [false, true] {
        let (_d, cfg) = fixture();
        let job = job(&cfg);
        pending(&job, 1, vec!["sam/r#7".into()]);
        due(&job);
        remember(&job, "sam/r#1\t1\t{}\n").unwrap();
        crate::state_reads::write(job.path("await.next"), "old unowned baseline").unwrap();
        if stopped {
            crate::state_reads::write(job.turn("1", "stopped"), "").unwrap();
        } else {
            job.set_turns(2).unwrap();
            crate::state_reads::write(job.turn("2", "outcome"), "failed").unwrap();
        }
        crate::watch::look(&cfg, "").unwrap();
        if stopped {
            assert_eq!(job.turns().as_deref(), Some("1"));
            assert!(job.path(PENDING).exists());
        } else {
            assert_eq!(job.turns().as_deref(), Some("2"));
            assert_eq!(read(&job.turn("2", "outcome")).as_deref(), Some("failed"));
            assert!(reset(&job, "2").is_none());
            assert!(captured(&job).unwrap().is_empty());
        }
    }
}

#[test]
fn replay_job_maximum_never_extends_the_failing_owners_installation() {
    let (d, mut cfg) = fixture();
    cfg.host_github_app = Some("host".into());
    let job = job(&cfg);
    let long = (now() + 7200) as u64;
    let mut refusal = Meter::new("host", "other-owner", now() as u64);
    refusal.retry_at = Some(long);
    refusal.record(&job.turn("1", "github-refusals")).unwrap();
    pending(&job, 1, vec!["sam/r#7".into()]);
    fs::write(d.path().join("limited"), "").unwrap();
    assert_eq!(replay(&cfg, &job).unwrap(), Some(long as i64));
    let observed = meters(&cfg);
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].owner, "sam");
    assert!(observed[0].retry_at.unwrap() < long);
}

#[test]
fn replay_records_only_its_own_reset_in_the_installation_meter() {
    let (d, mut cfg) = fixture();
    cfg.host_github_app = Some("host-app".into());
    let job = job(&cfg);
    crate::state_reads::write(job.path("link"), "sam/r#7").unwrap();
    fs::write(d.path().join("limited"), "").unwrap();
    // Another owner's long refusal widens the job's deadline.
    let wide = now() as u64 + 90_000;
    pending(&job, wide, vec!["sam/r#7".into()]);
    let parked = replay(&cfg, &job).unwrap().unwrap();
    assert_eq!(parked as u64, wide);
    let own: Vec<_> = meters(&cfg)
        .into_iter()
        .filter(|m| m.owner == "sam")
        .collect();
    assert_eq!(own.len(), 1);
    assert!(own[0].retry_at.unwrap() < wide, "job-wide maximum leaked");
}

#[test]
fn an_existing_future_hold_is_not_republished_by_a_watcher_pass() {
    let (_d, cfg) = fixture();
    let job = job(&cfg);
    let since = now() - 500;
    let until = now() + 3600;
    pending(&job, until as u64, vec!["sam/r#7".into()]);
    crate::state_reads::write(job.turn("1", LIMIT), format!("{since}\t{until}")).unwrap();
    let original = format!("{LIMIT}\t{since}\t{until}");
    crate::state_reads::write(job.path("await"), format!("{original}\n")).unwrap();
    crate::state_reads::write(job.turn("1", "end"), "await\n").unwrap();
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(read(&job.path("await")).as_deref(), Some(original.as_str()));
}
