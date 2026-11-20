use super::*;
use std::fs;

const REPO: &str = "sam/box";

struct Machine {
    dir: tempfile::TempDir,
    cfg: Config,
    settings: burrow_settings::Config,
}

#[derive(Default)]
struct Kicks(Vec<String>);

impl Wake for Kicks {
    fn steward(&mut self, repo: &str) -> Result<(), String> {
        self.0.push(repo.into());
        Ok(())
    }
}

const PATIENT: Patience = Patience {
    lock: Duration::from_secs(5),
};

impl Machine {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let p = |name: &str| dir.path().join(name);
        let settings = burrow_settings::Config {
            overlay: p("overlay.json"),
            allowed: vec![REPO.into(), "sam/web".into()],
            landing_record: Some(p("owners.json")),
            conaryd: vec![REPO.into()],
            ..Default::default()
        };
        fs::write(p("settings.json"), serde_json::to_vec(&settings).unwrap()).unwrap();
        // conaryd's events file as burrow renders it, loaded by Conary's own
        // loader, as the machine's build does.
        let template = serde_json::json!({
            "listen": "127.0.0.1:8091",
            "secret_path": p("secret"),
            "repos": [],
            "data": p("data"),
            "flow_config": p("flow.json"),
            "policy": p("policy.json"),
            "token_command": "/bin/false",
            "anchor_repo": null,
            "worker": "/bin/cn",
        });
        fs::write(p("events-template.json"), template.to_string()).unwrap();
        conary_github::flow::schema::load::<Runtime>(&fs::read(p("events-template.json")).unwrap())
            .unwrap();
        let cfg = Config {
            settings: p("settings.json"),
            steward_released: p("released.json"),
            data: p("data"),
            conaryd: Some(Conaryd {
                events_template: p("events-template.json"),
                events: p("events.json"),
                serial: p("events-serial"),
                started: p("conaryd-started"),
            }),
            steward_kick: String::new(),
        };
        Self { dir, cfg, settings }
    }

    fn set(&self, owner: &str) {
        self.settings
            .command(&["landing".into(), REPO.into(), owner.into()], "test")
            .unwrap();
    }

    fn run(&self, kicks: &mut Kicks) -> Moved {
        let moved = run(&self.cfg, kicks, &PATIENT).unwrap();
        self.exclusive();
        moved
    }

    fn holding(&self) -> Holding {
        Record::read(&self.dir.path().join("owners.json"))
            .unwrap()
            .holding(REPO)
    }

    /// The repos conaryd's events file gives it.
    fn given(&self) -> Vec<String> {
        let runtime: Runtime = read_json(&self.c().events).unwrap();
        runtime.repos.iter().map(ToString::to_string).collect()
    }

    /// Never both: the steward lands only what the record gives it, and
    /// conaryd is given only what the record gives conaryd.
    fn exclusive(&self) {
        let steward = self.holding().lands(Owner::Steward);
        let conaryd = self.given().iter().any(|r| r.eq_ignore_ascii_case(REPO));
        assert!(!(steward && conaryd), "both land {REPO}");
        let allowed = self.settings.decision(REPO).unwrap().allowed;
        assert_eq!(conaryd, self.holding().lands(Owner::Conaryd) && allowed);
    }

    /// conaryd started again on the events file as it is now, as
    /// conary-flow-given says once it has.
    /// conaryd started again, on the publication the serial counts, as
    /// conary-flow-given says once it has.
    fn started(&self) {
        let serial = fs::read_to_string(&self.c().serial).unwrap();
        fs::write(&self.c().started, serial.split_whitespace().next().unwrap()).unwrap();
    }

    fn c(&self) -> &Conaryd {
        self.cfg.conaryd.as_ref().unwrap()
    }

    fn release(&self, generation: u64) {
        fs::write(
            &self.cfg.steward_released,
            serde_json::json!({ REPO: generation }).to_string(),
        )
        .unwrap();
    }

    /// The flow's directory for the repo, as conaryd's pass opens it.
    fn flow_dir(&self) -> PathBuf {
        let data = self.dir.path().join("data");
        let repo: conary_core::op::Repo = REPO.parse().unwrap();
        let held = conary_github::flow::Data::open(&data, &repo).unwrap();
        held.path(0).parent().unwrap().to_path_buf()
    }
}

#[test]
fn the_steward_lets_go_before_conaryd_is_given_the_repo() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    assert_eq!(m.run(&mut kicks), Moved::default());
    assert!(m.holding().lands(Owner::Steward));

    m.set("conaryd");
    let moved = m.run(&mut kicks);
    assert_eq!(moved.started.get(REPO), Some(&Owner::Conaryd));
    assert!(moved.waiting.contains(REPO));
    let h = m.holding();
    assert_eq!(
        (h.owner, h.to, h.generation),
        (Owner::Steward, Some(Owner::Conaryd), 1)
    );
    assert!(m.given().is_empty());
    // The steward's pass is kicked so it lets go now, not at its next event.
    assert_eq!(kicks.0, [REPO]);

    // Until its marker names this handover, nobody lands.
    for stale in [None, Some(0)] {
        if let Some(g) = stale {
            m.release(g);
        }
        assert!(m.run(&mut kicks).waiting.contains(REPO));
        assert!(!m.holding().lands(Owner::Steward) && m.given().is_empty());
    }
    m.release(1);
    let moved = m.run(&mut kicks);
    assert_eq!(moved.moved.get(REPO), Some(&Owner::Conaryd));
    assert!(m.holding().lands(Owner::Conaryd));
    assert_eq!(m.given(), [REPO]);
    assert_eq!(
        kicks.0,
        [REPO],
        "the steward isn't kicked for a repo it gave away"
    );
}

#[test]
fn conaryd_lets_go_only_once_its_pass_and_workers_have_ended() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    m.release(1);
    m.run(&mut kicks);
    assert!(m.holding().lands(Owner::Conaryd));

    // A worker conaryd launched is still at work: its `.running` lock held.
    let dir = m.flow_dir();
    fs::write(dir.join("7-1.job"), b"{}").unwrap();
    let running = fs::File::create(dir.join("7-1.running")).unwrap();
    running.lock().unwrap();

    m.set("steward");
    // Taken off conaryd's events file at once, and given to nobody until
    // conaryd has started again on that file.
    assert!(m.run(&mut kicks).waiting.contains(REPO));
    assert!(m.given().is_empty() && !m.holding().lands(Owner::Steward));
    m.started();
    let cfg = m.cfg.clone();
    let run = std::thread::spawn(move || {
        let mut kicks = Kicks::default();
        let moved = run(&cfg, &mut kicks, &PATIENT).unwrap();
        (moved, kicks.0)
    });
    // And while the worker runs.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!run.is_finished());
    m.exclusive();
    assert!(m.given().is_empty());
    assert!(!m.holding().lands(Owner::Steward));

    running.unlock().unwrap();
    let (moved, kicked) = run.join().unwrap();
    assert_eq!(moved.moved.get(REPO), Some(&Owner::Steward));
    assert!(m.holding().lands(Owner::Steward));
    assert_eq!(kicked, [REPO]);
    m.exclusive();
}

#[test]
fn a_pass_conaryd_has_under_way_is_waited_for() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    m.release(1);
    m.run(&mut kicks);
    m.set("steward");
    m.run(&mut kicks);
    m.started();
    let data = m.dir.path().join("data");
    let repo: conary_core::op::Repo = REPO.parse().unwrap();
    let pass = conary_github::flow::Data::open(&data, &repo).unwrap();
    // A pass that doesn't end in time fails the run, and the repo stays
    // with nobody until the next.
    let impatient = Patience {
        lock: Duration::from_millis(300),
    };
    assert!(run(&m.cfg, &mut kicks, &impatient).is_err());
    m.exclusive();
    assert!(!m.holding().lands(Owner::Steward) && m.given().is_empty());
    let ends = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        drop(pass);
    });
    assert_eq!(m.run(&mut kicks).moved.get(REPO), Some(&Owner::Steward));
    ends.join().unwrap();
}

#[test]
fn a_run_cut_short_goes_on_from_the_record() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    // A restart: a fresh run reads the record, and doesn't start the
    // handover again (its generation stays, so the marker still counts).
    let again = m.run(&mut kicks);
    assert!(again.started.is_empty());
    assert_eq!(m.holding().generation, 1);
    m.release(1);
    assert_eq!(m.run(&mut kicks).moved.get(REPO), Some(&Owner::Conaryd));
    // And once it's done, runs change nothing: conaryd's events file is
    // written once, so it's restarted once and launches nothing twice.
    let written = fs::metadata(&m.c().events).unwrap().modified().unwrap();
    for _ in 0..3 {
        assert_eq!(m.run(&mut kicks), Moved::default());
    }
    assert_eq!(
        fs::metadata(&m.c().events).unwrap().modified().unwrap(),
        written
    );
}

#[test]
fn set_back_mid_handover_the_owner_carries_on_and_an_old_release_is_spent() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    m.set("steward");
    let moved = m.run(&mut kicks);
    assert!(moved.undone.contains(REPO));
    let h = m.holding();
    assert_eq!((h.owner, h.to, h.generation), (Owner::Steward, None, 2));
    assert!(h.lands(Owner::Steward));
    // The steward's release of the handover that was undone moves nothing.
    m.release(1);
    m.set("conaryd");
    assert!(m.run(&mut kicks).waiting.contains(REPO));
    assert_eq!(m.holding().generation, 3);
    assert!(m.given().is_empty());
    m.release(3);
    assert_eq!(m.run(&mut kicks).moved.get(REPO), Some(&Owner::Conaryd));
}

#[test]
fn a_repo_conaryd_may_not_have_stays_or_goes_back_to_the_steward() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    assert!(
        m.settings
            .command(
                &["landing".into(), "sam/web".into(), "conaryd".into()],
                "test"
            )
            .is_err()
    );
    m.set("conaryd");
    m.run(&mut kicks);
    m.release(1);
    m.run(&mut kicks);
    assert!(m.holding().lands(Owner::Conaryd));
    // A deploy takes it off conaryd's list: it goes back to the steward,
    // through the same handover.
    let narrower = burrow_settings::Config {
        conaryd: vec![],
        ..m.settings.clone()
    };
    fs::write(&m.cfg.settings, serde_json::to_vec(&narrower).unwrap()).unwrap();
    assert!(m.run(&mut kicks).waiting.contains(REPO));
    m.started();
    let moved = m.run(&mut kicks);
    assert_eq!(moved.moved.get(REPO), Some(&Owner::Steward));
    assert!(m.given().is_empty());
}

#[test]
fn a_pause_is_its_own_switch() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.settings
        .command(&["pause".into(), REPO.into()], "test")
        .unwrap();
    m.set("conaryd");
    m.run(&mut kicks);
    m.release(1);
    m.run(&mut kicks);
    // The repo changed hands and is still paused; the pause didn't move
    // it, and conaryd isn't given it while it lasts.
    let decision = m.settings.decision(REPO).unwrap();
    assert!(!decision.allowed);
    assert!(decision.landing.lands(Owner::Conaryd));
    assert!(m.given().is_empty());
    m.settings
        .command(&["allow".into(), REPO.into()], "test")
        .unwrap();
    assert_eq!(m.run(&mut kicks), Moved::default());
    assert_eq!(m.given(), [REPO]);
    // Paused again, conaryd isn't given it, and it still holds it.
    m.settings
        .command(&["pause".into(), REPO.into()], "test")
        .unwrap();
    assert_eq!(m.run(&mut kicks), Moved::default());
    assert!(m.given().is_empty() && m.holding().lands(Owner::Conaryd));
}

#[test]
fn a_free_lock_is_no_release_until_conaryd_has_started_without_the_repo() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    m.release(1);
    m.run(&mut kicks);
    // conaryd started on the file that gave it the repo.
    m.started();
    m.set("steward");
    // No pass and no worker: but a pass that read the old file may yet
    // take the lock, until conaryd has started again on the new one.
    for _ in 0..2 {
        assert!(m.run(&mut kicks).waiting.contains(REPO));
        assert!(!m.holding().lands(Owner::Steward) && m.given().is_empty());
    }
    m.started();
    assert_eq!(m.run(&mut kicks).moved.get(REPO), Some(&Owner::Steward));
}

#[test]
fn a_restart_counts_for_one_publication_only() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    m.release(1);
    m.run(&mut kicks);
    m.started();
    // Handed back: the file without the repo, and conaryd starts on it.
    m.set("steward");
    m.run(&mut kicks);
    m.started();
    let without = fs::read(&m.c().events).unwrap();
    // Set back before it's done, then handed back again: the file holds
    // what it held when conaryd last started, but it's a new publication,
    // and a pass that read the one in between may yet start.
    m.set("conaryd");
    assert!(m.run(&mut kicks).undone.contains(REPO));
    m.set("steward");
    assert!(m.run(&mut kicks).waiting.contains(REPO));
    assert_eq!(fs::read(&m.c().events).unwrap(), without);
    assert!(!m.holding().lands(Owner::Steward) && m.given().is_empty());
    m.started();
    assert_eq!(m.run(&mut kicks).moved.get(REPO), Some(&Owner::Steward));
}

#[test]
fn a_publication_cut_short_is_counted_before_it_can_count_as_a_restart() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    m.release(1);
    m.run(&mut kicks);
    m.started();
    let serial = fs::read(&m.c().serial).unwrap();
    m.set("steward");
    m.run(&mut kicks);
    // As if the run had been cut short between the file and its count:
    // conaryd's acknowledgment of the old count still matches the count.
    fs::write(&m.c().serial, &serial).unwrap();
    m.started();
    let moved = m.run(&mut kicks);
    assert!(
        moved.waiting.contains(REPO),
        "an old restart counted for a new file"
    );
    assert!(!m.holding().lands(Owner::Steward) && m.given().is_empty());
    m.started();
    assert_eq!(m.run(&mut kicks).moved.get(REPO), Some(&Owner::Steward));
}

#[test]
fn with_conaryd_gone_its_repo_comes_back_once_its_workers_have_exited() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    m.release(1);
    m.run(&mut kicks);
    assert!(m.holding().lands(Owner::Conaryd));
    // A deploy takes the last repo off burrow.landing.conaryd: conaryd no
    // longer runs, and burrow-landing still does.
    let dir = m.flow_dir();
    fs::write(dir.join("3-1.job"), b"{}").unwrap();
    let running = fs::File::create(dir.join("3-1.running")).unwrap();
    running.lock().unwrap();
    let narrower = burrow_settings::Config {
        conaryd: vec![],
        ..m.settings.clone()
    };
    fs::write(&m.cfg.settings, serde_json::to_vec(&narrower).unwrap()).unwrap();
    let cfg = Config {
        conaryd: None,
        ..m.cfg.clone()
    };
    let run = std::thread::spawn(move || run(&cfg, &mut Kicks::default(), &PATIENT).unwrap());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!run.is_finished());
    assert!(!m.holding().lands(Owner::Steward));
    running.unlock().unwrap();
    assert_eq!(run.join().unwrap().moved.get(REPO), Some(&Owner::Steward));
}

#[test]
fn a_kick_that_fails_moves_the_repo_all_the_same() {
    struct Down;
    impl Wake for Down {
        fn steward(&mut self, repo: &str) -> Result<(), String> {
            Err(format!("kicking {repo}: refused"))
        }
    }
    let m = Machine::new();
    m.set("conaryd");
    run(&m.cfg, &mut Down, &PATIENT).unwrap();
    m.release(1);
    run(&m.cfg, &mut Down, &PATIENT).unwrap();
    m.set("steward");
    run(&m.cfg, &mut Down, &PATIENT).unwrap();
    m.started();
    let moved = run(&m.cfg, &mut Down, &PATIENT).unwrap();
    assert_eq!(moved.moved.get(REPO), Some(&Owner::Steward));
}

#[test]
fn nothing_moves_on_what_cant_be_read() {
    let m = Machine::new();
    let mut kicks = Kicks::default();
    m.set("conaryd");
    m.run(&mut kicks);
    fs::write(&m.cfg.steward_released, b"not json").unwrap();
    assert!(run(&m.cfg, &mut kicks, &PATIENT).is_err());
    fs::remove_file(&m.cfg.steward_released).unwrap();
    fs::write(m.dir.path().join("owners.json"), b"not json").unwrap();
    let before = fs::read(&m.c().events).unwrap();
    assert!(run(&m.cfg, &mut kicks, &PATIENT).is_err());
    assert_eq!(fs::read(&m.c().events).unwrap(), before);
}
