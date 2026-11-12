use super::tests::fixture;
use super::*;
use crate::engine::Engine;

fn pending(job: &Job, due: u64, targets: Vec<String>) {
    swap(
        job.path(PENDING),
        serde_json::to_vec(&Pending {
            targets,
            retry_at: due,
            turn: job.turns().unwrap(),
        })
        .unwrap(),
    )
    .unwrap();
    park(job, due as i64).unwrap();
    swap(job.turn("1", "outcome"), "GitHub quota; waiting for reset").unwrap();
}
fn due(job: &Job) {
    swap(job.path("await"), format!("{LIMIT}\t100\t1\n")).unwrap();
}
fn job(cfg: &Config) -> Job<'_> {
    let job = Job::new(cfg, "recovery");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    job
}

#[test]
fn ordinary_replay_errors_retire_only_after_an_accepted_resume() {
    for problem in ["denied", "invalid", "closed"] {
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
        };
        crate::ask::record(&job, &recommendation).unwrap();
        crate::ask::finish(&cfg, &job, "ask").unwrap();
        assert!(crate::ask::load(&job).is_some());
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
        if another_await {
            assert!(restored.contains("sam/r#8\t"));
        }
        assert_eq!(job.turns().as_deref(), Some("1"));
        crate::watch::look(&cfg, "").unwrap();
        assert_eq!(job.turns().as_deref(), Some("2"));
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
    swap(job.path(PENDING), intent).unwrap(); // crash before retiring intent
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
            swap(job.path("app"), app).unwrap();
        }
        swap(job.turn("1", "engine"), Engine::Codex.name()).unwrap();
        swap(job.path("link"), "sam/r#7").unwrap();
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
        for e in fs::read_dir(job.path("github"))
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
    fs::create_dir(job.turn("1", "end")).unwrap_err(); // existing completed declaration
    fs::remove_file(job.turn("1", "end")).unwrap();
    fs::create_dir(job.turn("1", "end")).unwrap();
    due(&job);
    crate::watch::look(&cfg, "").unwrap();
    assert_eq!(job.turns().as_deref(), Some("1"));
    assert!(job.path(PENDING).exists());
    assert!(read(&job.path("await")).unwrap().starts_with(LIMIT));
    let captured = read(&job.path("await.next")).unwrap();
    fs::remove_dir(job.turn("1", "end")).unwrap();
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
    let original = captured(&job);
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
