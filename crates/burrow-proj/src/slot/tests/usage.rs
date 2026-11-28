use super::*;

#[test]
fn fresh_meter_runs_the_held_turn_and_closes_its_notice() {
    use burrow_usage_reset::meter::{FRESH, Snapshot, Window};
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let notices = jobs.join("notices");
    burrow_test::executable(
        jobs.join("notify"),
        format!("#!/bin/sh\necho \"$1 $2\" >> '{}'\n", notices.display()),
    );
    let cfg = config(&jobs, 1);
    let t = now();
    crate::usage::record(
        &cfg,
        "w0",
        Some(t + 7200),
        "usage limit",
        t - FRESH as i64 - 100,
    )
    .unwrap();
    let _kid = waiter(&jobs, &tmp, "w0", 1);
    until("held turn joins the queue", || in_queue(&jobs) == 1);
    let mut meter = Snapshot {
        windows: [(
            "seven_day".into(),
            Window {
                used: 1.0,
                resets_at: 0.0,
            },
        )]
        .into(),
        credits: false,
        read_at: t as f64 - FRESH - 1.0,
        requested_at: Some(t as f64 - FRESH - 1.0),
    };
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    sleep(Duration::from_secs(3));
    assert_eq!(took(&jobs, "w0"), None, "stale meter keeps the hold");
    crate::usage::record(&cfg, "w0", Some(t + 7200), "usage limit", t - 10).unwrap();
    meter.read_at = (t - 11) as f64;
    meter.requested_at = Some(meter.read_at);
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    sleep(Duration::from_secs(3));
    assert_eq!(took(&jobs, "w0"), None, "pre-refusal meter keeps the hold");
    meter.read_at = now() as f64;
    meter.requested_at = Some(meter.read_at);
    meter
        .record(cfg.meters.as_ref().unwrap(), "claude")
        .unwrap();
    until("fresh meter lets the held turn run", || {
        took(&jobs, "w0").is_some()
    });
    assert!(crate::usage::out(&cfg).is_none());
    assert!(
        fs::read_to_string(notices)
            .unwrap()
            .contains("close proj-usage-claude")
    );
}

// This machine's login is a credential a sandbox doesn't use (usage/login.rs):
// while its hold stands, the turns here wait in line, and a sandbox's takes
// the free slot past them, as its own --unstick turn does.
#[test]
fn a_sandboxs_turn_takes_a_slot_past_this_machines_login_hold() {
    use crate::usage::login::LOGGED_OUT;
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 1);
    crate::usage::record(&cfg, "here", None, LOGGED_OUT, now()).unwrap();
    let _here = waiter(&jobs, &tmp, "here", 1);
    until("the turn here joins the line", || in_queue(&jobs) == 1);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&jobs, "here"), None, "the login hold holds it");
    // Room for a sandbox's turn, with the one here in line, and none for
    // another here.
    assert!(crate::slot::room(&cfg, "claude", true));
    assert!(!crate::slot::room(&cfg, "claude", false));
    fs::create_dir_all(jobs.join("vm")).unwrap();
    fs::write(jobs.join("vm/agent"), "web-ab12\n").unwrap();
    let _vm = waiter(&jobs, &tmp, "vm", 1);
    until("the sandbox's turn takes the free slot", || {
        took(&jobs, "vm").is_some()
    });
    assert_eq!(took(&jobs, "here"), None);
    assert!(crate::usage::out(&cfg).is_some(), "the hold still stands");

    fs::create_dir_all(jobs.join("u")).unwrap();
    fs::write(jobs.join("u/project"), "/src/web\n").unwrap();
    fs::write(jobs.join("u/unstick"), "").unwrap();
    fs::write(jobs.join("u/agent"), "web-cd34\n").unwrap();
    admission::bind_fixture(&jobs, 1);
    let unstick = std::thread::spawn(move || take(&cfg, "u", "1", Class::Normal, true).unwrap());
    until("the sandbox's --unstick turn takes its own slot", || {
        unstick.is_finished()
    });
    let slot = unstick.join().unwrap();
    assert_eq!(
        fs::read_to_string(jobs.join("u/1.slot")).unwrap(),
        "unstick\n"
    );
    slot.unlock().unwrap();
}

// A queued turn whose role moves off a held engine in live settings must not
// stay stuck behind the old engine's usage hold: slot admission refreshes its
// choice from host-owned settings and lets it take a free slot immediately.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy test")]
fn a_queued_turn_moves_off_a_held_engine_when_its_role_reroutes() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let mut v = example();
    v["jobs"] = jobs.to_str().unwrap().into();
    v["meters"] = serde_json::json!(jobs.join("meters"));
    v["settings"]["overlay"] = serde_json::json!(jobs.join("settings.json"));
    v["settings"]["engines"] = serde_json::json!(["claude", "alt-engine"]);
    let notify = jobs.join("notify");
    burrow_test::executable(&notify, "#!/bin/sh\nexit 0\n");
    v["notify"] = serde_json::json!(notify);
    v["maxJobs"] = 1.into();
    v["pipelineSlots"] = 0.into();
    v["maxJobsPerProject"] = 4.into();
    v["minFreeMemory"] = 0.into();
    v["maxWait"] = 99999.into();
    v["flake"] = "/nowhere".into();
    v["tools"]["git"] = "git".into();
    let cfg: Config = serde_json::from_value(v).unwrap();
    let settings = cfg.settings.as_ref().unwrap();
    settings
        .command(
            &[
                "offer".into(),
                "alt-engine".into(),
                "models".into(),
                "alt-model".into(),
            ],
            "test",
        )
        .unwrap();
    settings
        .command(
            &[
                "offer".into(),
                "alt-engine".into(),
                "efforts".into(),
                "high".into(),
            ],
            "test",
        )
        .unwrap();
    settings
        .command(&["role".into(), "inbox".into(), "claude".into()], "test")
        .unwrap();
    crate::usage::record(&cfg, "w0", Some(now() + 7200), "usage limit", now()).unwrap();

    let job = Job::new(&cfg, "w0");
    fs::create_dir_all(&job.dir).unwrap();
    let project = tmp.path().join("web");
    fs::create_dir_all(&project).unwrap();
    crate::policy::bind(
        &job,
        crate::policy::Identity::Local,
        crate::policy::Kind::Manual,
    )
    .unwrap();
    let req = crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        role: Some("inbox".into()),
        ..Default::default()
    };
    req.save(&job).unwrap();
    job.set_turns(1).unwrap();
    let initial = crate::engine::Choice {
        engine: "claude".into(),
        model: None,
        effort: None,
    };
    crate::retry::Identity::capture_choice(&job, "1", &initial).unwrap();
    job.record_choice("1", &initial).unwrap();

    let wait_cfg = cfg.clone();
    let waiter =
        std::thread::spawn(move || take(&wait_cfg, "w0", "1", Class::Normal, false).unwrap());
    until("held turn joins the queue", || in_queue(&jobs) == 1);
    sleep(Duration::from_secs(3));
    assert!(!waiter.is_finished(), "Claude hold keeps it waiting");

    settings
        .command(
            &["role".into(), "inbox".into(), "alt-engine".into()],
            "test",
        )
        .unwrap();
    settings
        .command(
            &[
                "model".into(),
                "inbox".into(),
                "alt-engine".into(),
                "alt-model".into(),
                "high".into(),
            ],
            "test",
        )
        .unwrap();

    until(
        "rerouted turn leaves the Claude hold and takes a slot",
        || waiter.is_finished(),
    );
    let slot = waiter.join().unwrap();
    assert!(jobs.join("w0/1.slot").exists());
    let updated = crate::retry::Identity::load(&job, "1")
        .unwrap()
        .unwrap()
        .choice;
    assert_eq!(updated.engine, "alt-engine");
    assert_eq!(updated.model.as_deref(), Some("alt-model"));
    assert_eq!(updated.effort.as_deref(), Some("high"));
    assert_eq!(crate::usage::login::run_of(&cfg, "w0").engine, "alt-engine");
    slot.unlock().unwrap();
}
