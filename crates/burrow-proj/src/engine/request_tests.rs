//! A job's own writes to its metadata never decide what it runs as: not on
//! an ordinary resume, nor on an engine restart within a turn. Only the
//! starter and host-side changes it authorizes do.
use super::*;
use crate::job::Request;

struct Fixture {
    _dir: tempfile::TempDir,
    cfg: Config,
    project: std::path::PathBuf,
    home: std::path::PathBuf,
    root: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    cfg.jobs = root.join("jobs");
    cfg.projects = root.join("projects");
    cfg.scratch = root.join("scratch");
    cfg.waits = None;
    cfg.meters = None;
    cfg.tools.git = "git".into();
    cfg.tools.systemd_run = "true".into();
    cfg.tools.systemctl = "false".into();
    // The App it was started as has no environment here, so a turn stops
    // at the App it would act as, and names it, before it takes a slot.
    cfg.apps = [("workers".to_owned(), root.join("workers.env"))].into();
    let project = cfg.projects.join("project");
    fs::create_dir_all(&project).unwrap();
    crate::config::tests::set(&mut cfg, &root, &["role work codex", "role wide claude"]);
    Fixture {
        project,
        home: root.join("home"),
        root,
        cfg,
        _dir: dir,
    }
}

/// What a Codex job's sandbox can do to its own job directory.
fn tamper(job: &Job, root: &Path) {
    fs::write(job.path("engine"), "claude").unwrap();
    fs::write(job.path("role"), "wide\n").unwrap();
    fs::write(job.path("project"), format!("{}\n", root.display())).unwrap();
    fs::write(job.path("app"), "other\n").unwrap();
    let _ = fs::remove_file(job.path("owned"));
}

fn workspace(f: &Fixture, job: &Job, run: &str) -> Vec<std::path::PathBuf> {
    let home = f.home.clone();
    let env = move |name: &str| (name == "HOME").then(|| home.clone().into_os_string());
    let machine = sandbox::tests::machine_in(&f.root.join(run));
    capabilities::resolve(&f.cfg, job, &f.root.join("scratch/job"), &env, &machine)
        .unwrap()
        .items
        .into_iter()
        .find(|g| g.capability == capabilities::Capability::Workspace)
        .unwrap()
        .paths
        .into_iter()
        .map(|(p, _)| p)
        .collect()
}

#[test]
fn a_jobs_own_writes_never_change_what_it_runs_as() {
    let f = fixture();
    let new = crate::cmd::New {
        role: Some("work".into()),
        app: Some("workers".into()),
        owned: true,
        kicks: vec!["starter".into()],
        local_work: true,
        ..Default::default()
    };
    let id = crate::cmd::start_job(&f.cfg, &new, "project", "the task").unwrap();
    let job = Job::new(&f.cfg, &id);
    fs::write(job.path("session"), "s\n").unwrap();
    let started = Request::load(&job).unwrap();
    assert_eq!(started.project, f.project.to_string_lossy());
    assert_eq!(started.role.as_deref(), Some("work"));
    assert_eq!(started.app.as_deref(), Some("workers"));
    assert!(started.owned && started.engine.is_none() && started.agent.is_none());

    // Turn 1 runs, and its sandbox rewrites the job's metadata.
    tamper(&job, &f.root);
    fs::write(job.path("agent"), "a-sandbox\n").unwrap();
    // A same-turn engine restart resolves its grants again: the original
    // workspace, not the root the job named.
    let paths = workspace(&f, &job, "run1");
    assert!(paths.contains(&f.project), "{paths:?}");
    assert!(!paths.contains(&f.root), "{paths:?}");
    assert_eq!(Request::load(&job).unwrap(), started);

    // An ordinary resume: the next turn, from a person or the watcher.
    crate::cmd::reply(&f.cfg, &id, "carry on", false, None).unwrap();
    assert_eq!(job.turns().as_deref(), Some("2"));
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Codex);
    assert!(crate::turn::exec(&f.cfg, &id, "normal").is_err());
    // It stopped at the App it was started as, not the one it wrote, on
    // the engine its role gives it, with its published copy put back.
    let outcome = crate::read(&job.turn("2", "outcome")).unwrap();
    assert!(outcome.contains("GitHub App workers"), "{outcome}");
    assert_eq!(
        crate::read(&job.turn("2", "engine")).as_deref(),
        Some("codex")
    );
    assert_eq!(
        crate::retry::Identity::load(&job, "2")
            .unwrap()
            .unwrap()
            .choice
            .engine,
        Engine::Codex
    );
    assert_eq!(crate::read(&job.path("role")).as_deref(), Some("work"));
    assert_eq!(crate::read(&job.path("app")).as_deref(), Some("workers"));
    assert!(job.path("owned").exists() && !job.path("engine").exists());
    assert!(!job.path("agent").exists());
    assert_eq!(
        crate::read(&job.path("project")).as_deref(),
        Some(f.project.to_str().unwrap())
    );

    // A restart within turn 2 after more writes: the same boundary.
    tamper(&job, &f.root);
    let paths = workspace(&f, &job, "run2");
    assert!(paths.contains(&f.project) && !paths.contains(&f.root));
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Codex);
    assert!(Request::load(&job).unwrap().owned);
}

#[test]
fn host_authorized_changes_and_machine_settings_still_apply() {
    let mut f = fixture();
    let new = crate::cmd::New {
        engine: Some(Engine::Codex),
        local_work: true,
        ..Default::default()
    };
    let id = crate::cmd::start_job(&f.cfg, &new, "project", "the task").unwrap();
    let job = Job::new(&f.cfg, &id);
    fs::write(job.path("session"), "s\n").unwrap();
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Codex);
    // The job drops its label's pin itself: nothing changes.
    fs::remove_file(job.path("engine")).unwrap();
    crate::cmd::reply(&f.cfg, &id, "next", false, None).unwrap();
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Codex);
    // The inbox maps its label to a role: that role's engine from now on,
    // as the machine's roles file has it on each turn.
    Request::enroll(&job, "wide".into()).unwrap();
    crate::cmd::reply(&f.cfg, &id, "again", false, None).unwrap();
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Claude);
    assert_eq!(crate::read(&job.path("role")).as_deref(), Some("wide"));
    let root = f.root.clone();
    crate::config::tests::set(
        &mut f.cfg,
        &root,
        &[
            "role wide codex",
            "offer codex models chosen",
            "model wide codex chosen high",
        ],
    );
    let job = Job::new(&f.cfg, &id);
    crate::cmd::reply(&f.cfg, &id, "once more", false, None).unwrap();
    let c = choice(&f.cfg, &job).unwrap();
    assert_eq!(
        (c.engine, c.model.as_deref(), c.effort.as_deref()),
        (Engine::Codex, Some("chosen"), Some("high"))
    );
}

#[test]
fn a_job_from_before_the_record_adopts_its_request_once() {
    let f = fixture();
    let job = Job::new(&f.cfg, "legacy");
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(job.path("project"), format!("{}\n", f.project.display())).unwrap();
    fs::write(job.path("role"), "work\n").unwrap();
    fs::write(job.path("app"), "workers\n").unwrap();
    job.set_turns(1).unwrap();
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Codex);
    tamper(&job, &f.root);
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Codex);
    let request = Request::load(&job).unwrap();
    assert_eq!(request.app.as_deref(), Some("workers"));
    assert_eq!(request.project, f.project.to_string_lossy());

    // One whose retained retry captured its files on the host adopts
    // those, not what the job wrote since.
    let job = Job::new(&f.cfg, "retained");
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    crate::ask::ensure(&job).unwrap();
    fs::write(
        job.turn("1", "execution"),
        serde_json::json!({
            "choice": {"engine": "codex", "model": null, "effort": null},
            "files": {"project": f.project.to_str(), "role": "work", "app": "workers",
                      "engine": null, "agent": null, "owned": ""},
        })
        .to_string(),
    )
    .unwrap();
    fs::write(job.path("owned"), "").unwrap();
    tamper(&job, &f.root);
    let request = Request::load(&job).unwrap();
    assert_eq!(request.role.as_deref(), Some("work"));
    assert_eq!(request.app.as_deref(), Some("workers"));
    assert_eq!(request.project, f.project.to_string_lossy());
    assert!(request.owned && request.engine.is_none());
}

#[test]
fn a_role_is_scoped_to_the_bound_repository_not_a_rewritten_origin() {
    let mut f = fixture();
    let git = |args: &[&str]| {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(&f.project)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    };
    git(&["init", "-q"]);
    git(&["remote", "add", "origin", "https://github.com/Sam/Code.git"]);
    let root = f.root.clone();
    crate::config::tests::set(
        &mut f.cfg,
        &root,
        &["role work claude", "role work@sam/code codex"],
    );
    let new = crate::cmd::New {
        role: Some("work".into()),
        ..Default::default()
    };
    let id = crate::cmd::start_job(&f.cfg, &new, "project", "the task").unwrap();
    let job = Job::new(&f.cfg, &id);
    fs::write(job.path("session"), "s\n").unwrap();
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Codex);
    // The job's git metadata grant reaches its origin.
    git(&[
        "remote",
        "set-url",
        "origin",
        "https://github.com/else/where.git",
    ]);
    crate::cmd::reply(&f.cfg, &id, "next", false, None).unwrap();
    assert_eq!(choice(&f.cfg, &job).unwrap().engine, Engine::Codex);
}
