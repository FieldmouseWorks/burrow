use super::*;
use crate::{config::Config, job::Request, pick::Class};
use std::fs;

/// Jobs, a project and the machine's flake, in a scratch dir.
fn rig() -> (tempfile::TempDir, Config) {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut v = crate::config::tests::example();
    v["jobs"] = tmp.path().join("jobs").to_str().unwrap().into();
    let mut cfg: Config = serde_json::from_value(v).unwrap();
    cfg.host_state = None;
    cfg.tools.git = "git".into();
    cfg.flake = tmp.path().join("flake").to_str().unwrap().into();
    for d in ["jobs", "flake", "web"] {
        fs::create_dir(tmp.path().join(d)).unwrap();
    }
    (tmp, cfg)
}

fn at(tmp: &tempfile::TempDir, d: &str) -> String {
    tmp.path().join(d).to_str().unwrap().to_owned()
}

/// A job as its starter makes it: its request, then its schedule.
fn started<'a>(cfg: &'a Config, id: &'a str, project: &str, schedule: &Schedule) -> Job<'a> {
    let job = Job::new(cfg, id);
    fs::create_dir(&job.dir).unwrap();
    Request {
        project: project.into(),
        ..Request::default()
    }
    .save(&job)
    .unwrap();
    schedule.save(&job).unwrap();
    job
}

/// What a sandbox can do to the published copy: turn on what the starter
/// left off, take away what it gave, and rewrite every value.
fn tamper(job: &Job, given: &Schedule, project: &str) {
    for flag in ["unstick", "pipeline"] {
        let path = job.path(flag);
        if path.exists() {
            fs::remove_file(path).unwrap();
        } else {
            fs::write(path, "").unwrap();
        }
    }
    fs::write(job.path("timeout"), "0\n").unwrap();
    if given.idle.is_some() {
        fs::remove_file(job.path("idle")).unwrap();
    } else {
        fs::write(job.path("idle"), "1s\n").unwrap();
    }
    fs::write(job.path("kick"), "proj-watch\nautoland\nelsewhere\n").unwrap();
    fs::write(job.path("link"), "https://github.com/sam/other/issues/9\n").unwrap();
    fs::write(job.path("project"), format!("{project}\n")).unwrap();
}

/// Each of a job's schedule files, written by the job itself, changes
/// nothing its next turn is scheduled or bounded by: not its record, not
/// what a turn restores and runs on, not its project, class or demotion as
/// the line sees them, and the copy goes back as the starter had it.
#[test]
fn a_jobs_writes_to_its_schedule_files_change_nothing_at_its_next_turn() {
    // A pipeline job in the flake would go as one; in web it's steward.
    let (tmp, cfg) = rig();
    let (web, flake) = (at(&tmp, "web"), at(&tmp, "flake"));
    let ordinary = Schedule {
        timeout: Some("1h".into()),
        kick: vec!["autoland".into()],
        link: Some("https://github.com/sam/web/issues/2".into()),
        ..Schedule::default()
    };
    let urgent = Schedule {
        unstick: true,
        pipeline: true,
        idle: Some("10m".into()),
        ..Schedule::default()
    };
    for (id, given) in [("ordinary", ordinary), ("urgent", urgent)] {
        let job = started(&cfg, id, &web, &given);
        let published: Vec<_> = ["unstick", "pipeline", "timeout", "idle", "kick", "link"]
            .iter()
            .map(|k| (k, fs::read(job.path(k)).ok()))
            .collect();
        let class = crate::pick::Classes::default().class(&cfg, &job, Class::Normal);
        let demoted = crate::pick::demoted(&cfg, &job);
        assert_eq!(class == Class::Steward, given.pipeline, "{id}");

        tamper(&job, &given, &flake);

        assert_eq!(Schedule::load(&job).unwrap(), given, "{id}: the record");
        assert_eq!(Schedule::of(&job), given, "{id}: a look");
        assert_eq!(crate::pick::project(&job), web, "{id}: its project");
        assert_eq!(
            crate::pick::Classes::default().class(&cfg, &job, Class::Normal),
            class,
            "{id}: its class"
        );
        assert_eq!(crate::pick::demoted(&cfg, &job), demoted, "{id}: demotion");
        let facts = {
            job.set_turns(1).unwrap();
            job.facts().unwrap()
        };
        assert_eq!(facts.unstick, given.unstick, "{id}: the watchdog's look");
        assert_eq!(facts.link, given.link, "{id}: the watchdog's link");

        // The next turn runs on the record, its copy put back first.
        assert_eq!(Schedule::restore(&job).unwrap(), given, "{id}: the turn");
        for (key, bytes) in &published {
            assert_eq!(&fs::read(job.path(key)).ok(), bytes, "{id}: {key}'s copy");
        }
    }
}

/// `proj unstick` is the host-side change the starter authorizes: it makes
/// the record --unstick, and a job removing the copy undoes nothing.
#[test]
fn proj_unstick_changes_the_record_and_the_copy_follows() {
    let (tmp, cfg) = rig();
    let job = started(&cfg, "stuck", &at(&tmp, "web"), &Schedule::default());
    Schedule::unstick(&job).unwrap();
    assert!(job.path("unstick").exists());
    fs::remove_file(job.path("unstick")).unwrap();
    assert!(Schedule::of(&job).unstick);
    Schedule::restore(&job).unwrap();
    assert!(job.path("unstick").exists());
}

/// A job from before the record adopts its published files once; after
/// that, what it writes is only its copy.
#[test]
fn a_legacy_job_adopts_its_files_once() {
    let (_tmp, cfg) = rig();
    let job = Job::new(&cfg, "legacy");
    fs::create_dir(&job.dir).unwrap();
    fs::write(job.path("timeout"), "2h\n").unwrap();
    fs::write(job.path("kick"), "autoland\nproj-watch\n").unwrap();
    fs::write(job.path("pipeline"), "").unwrap();
    let adopted = Schedule::load(&job).unwrap();
    assert_eq!(
        adopted,
        Schedule {
            pipeline: true,
            timeout: Some("2h".into()),
            kick: vec!["autoland".into(), "proj-watch".into()],
            ..Schedule::default()
        }
    );
    fs::write(job.path("unstick"), "").unwrap();
    fs::write(job.path("timeout"), "0\n").unwrap();
    assert_eq!(Schedule::load(&job).unwrap(), adopted);
}
