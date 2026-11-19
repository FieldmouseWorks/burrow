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
            events_template: p("events-template.json"),
            events: p("events.json"),
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
        let runtime: Runtime = read_json(&self.cfg.events).unwrap();
        runtime.repos.iter().map(ToString::to_string).collect()
    }

    /// Never both: the steward lands only what the record gives it, and
    /// conaryd is given only what the record gives conaryd.
    fn exclusive(&self) {
        let steward = self.holding().lands(Owner::Steward);
        let conaryd = self.given().iter().any(|r| r.eq_ignore_ascii_case(REPO));
        assert!(!(steward && conaryd), "both land {REPO}");
        assert_eq!(conaryd, self.holding().lands(Owner::Conaryd));
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
    let cfg = m.cfg.clone();
    let run = std::thread::spawn(move || {
        let mut kicks = Kicks::default();
        let moved = run(&cfg, &mut kicks, &PATIENT).unwrap();
        (moved, kicks.0)
    });
    // It's taken off conaryd's events file at once, and given to nobody
    // while the worker runs.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while m.holding().to.is_none() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
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
    let written = fs::metadata(&m.cfg.events).unwrap().modified().unwrap();
    for _ in 0..3 {
        assert_eq!(m.run(&mut kicks), Moved::default());
    }
    assert_eq!(
        fs::metadata(&m.cfg.events).unwrap().modified().unwrap(),
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
    // The repo changed hands and is still paused; the pause didn't move it.
    let decision = m.settings.decision(REPO).unwrap();
    assert!(!decision.allowed);
    assert!(decision.landing.lands(Owner::Conaryd));
    m.settings
        .command(&["allow".into(), REPO.into()], "test")
        .unwrap();
    assert_eq!(m.run(&mut kicks), Moved::default());
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
    let before = fs::read(&m.cfg.events).unwrap();
    assert!(run(&m.cfg, &mut kicks, &PATIENT).is_err());
    assert_eq!(fs::read(&m.cfg.events).unwrap(), before);
}
