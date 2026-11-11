use super::*;
use crate::{config::tests::example, pick::Class, slot, usage};
use std::fs::File;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::Instant;
use tempfile::TempDir;

#[allow(dead_code)]
#[path = "fixtures/pick.rs"]
mod legacy_pick;
#[allow(dead_code)]
#[path = "fixtures/slot.rs"]
mod legacy_slot;

const MODE: &str = "BURROW_RETRY_TEST_MODE";
const ROOT: &str = "BURROW_RETRY_TEST_ROOT";

fn config(root: &Path) -> Config {
    serde_json::from_slice(&fs::read(root.join("config.json")).unwrap()).unwrap()
}

struct Rig {
    tmp: TempDir,
    cfg: Config,
    children: Vec<Child>,
}
impl Drop for Rig {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        for entry in fs::read_dir(&self.cfg.jobs).unwrap().filter_map(Result::ok) {
            if let Some(pid) = read(&entry.path().join("pid")).and_then(|s| s.parse::<i32>().ok())
                && let Some(pid) = rustix::process::Pid::from_raw(pid)
            {
                let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
            }
        }
    }
}

fn rig() -> Rig {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let jobs = root.join("jobs");
    fs::create_dir_all(jobs.join(".queue")).unwrap();
    fs::create_dir_all(jobs.join(".slots")).unwrap();
    fs::create_dir(root.join("old")).unwrap();
    burrow_test::executable(
        root.join("old/burrow-proj"),
        fs::read(std::env::current_exe().unwrap()).unwrap(),
    );
    let systemctl = root.join("systemctl");
    burrow_test::executable(
        &systemctl,
        format!(
            r#"#!/bin/sh
shift
[ "$1" != -q ] || shift
case "$1" in
 show) prop=$3; unit=$4 ;;
 is-active) unit=$2 ;;
 stop|reset-failed) unit=$2 ;;
esac
id=${{unit#proj-}}; id=${{id%.service}}
pidfile='{jobs}'/$id/pid
pid=$(cat "$pidfile" 2>/dev/null)
case "$1:$prop" in
 show:MainPID) echo "$pid" ;;
 show:LoadState) if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then echo loaded; else echo not-found; fi ;;
 is-active:) [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; exit $? ;;
 stop:) [ -z "$pid" ] || kill "$pid" 2>/dev/null; rm -f "$pidfile" ;;
esac
exit 0
"#,
            jobs = jobs.display()
        ),
    );
    let run = root.join("systemd-run");
    burrow_test::executable(
        &run,
        format!(
            r#"#!/bin/sh
while [ "$#" -gt 0 ] && [ "$1" != _exec ]; do shift; done
shift
id=$1
BURROW_RETRY_TEST_MODE=new BURROW_RETRY_TEST_ROOT='{root}' \
 '{exe}' --exact retry::tests::child --nocapture --test-threads=1 _exec "$id" normal </dev/null >'{root}'/new-$id.log 2>&1 &
echo $! >'{jobs}'/$id/pid
"#,
            root = root.display(),
            exe = std::env::current_exe().unwrap().display(),
            jobs = jobs.display()
        ),
    );
    let mut value = example();
    value["jobs"] = jobs.to_str().unwrap().into();
    value["maxJobs"] = 6.into();
    value["pipelineSlots"] = 1.into();
    value["maxJobsPerProject"] = 6.into();
    value["usageTrickle"] = 2.into();
    value["minFreeMemory"] = 0.into();
    value["notify"] = serde_json::Value::Null;
    value["waits"] = serde_json::Value::Null;
    value["inbox"] = serde_json::Value::Null;
    value["tools"]["git"] = "git".into();
    value["tools"]["systemctl"] = systemctl.to_str().unwrap().into();
    value["tools"]["systemdRun"] = run.to_str().unwrap().into();
    fs::write(
        root.join("config.json"),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
    let cfg = config(root);
    Rig {
        tmp,
        cfg,
        children: Vec::new(),
    }
}

fn until(mut f: impl FnMut() -> bool) {
    let began = Instant::now();
    while !f() {
        assert!(began.elapsed() < Duration::from_secs(20), "timed out");
        sleep(Duration::from_millis(20));
    }
}

fn start(r: &mut Rig, id: &str, mode: &str, project: &str) {
    let job = Job::new(&r.cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    fs::write(job.path("project"), format!("{project}\n")).unwrap();
    fs::write(job.path("turns"), "1\n").unwrap();
    fs::write(job.path("prompt.1"), "the original prompt\n").unwrap();
    fs::write(job.path("session"), "conversation-123\n").unwrap();
    fs::write(
        job.turn("1", "engine"),
        if matches!(mode, "old-codex" | "new-codex") {
            "codex\n"
        } else {
            "claude\n"
        },
    )
    .unwrap();
    if mode == "old-unstick" {
        fs::write(job.path("unstick"), "").unwrap();
    }
    fs::write(
        job.turn("1", "deadline"),
        format!("{}\n", now_nanos() + 60_000_000_000),
    )
    .unwrap();
    fs::write(job.turn("1", "jsonl"), "{\"type\":\"result\",\"is_error\":true,\"result\":\"You've hit your monthly spend limit\"}\n").unwrap();
    fs::write(job.path("engine"), read(&job.turn("1", "engine")).unwrap()).unwrap();
    Identity::capture(&job, "1", engine::choice(&r.cfg, &job).unwrap()).unwrap();
    let exe = if mode.starts_with("old") {
        r.tmp.path().join("old/burrow-proj")
    } else {
        std::env::current_exe().unwrap()
    };
    let child = Command::new(exe)
        .args([
            "--exact",
            "retry::tests::child",
            "--nocapture",
            "--test-threads=1",
            "_exec",
            id,
            "normal",
        ])
        .env(MODE, mode)
        .env(ROOT, r.tmp.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    fs::write(job.path("pid"), child.id().to_string()).unwrap();
    r.children.push(child);
}

#[test]
fn child() {
    let Ok(mode) = std::env::var(MODE) else {
        return;
    };
    let cfg = config(Path::new(&std::env::var(ROOT).unwrap()));
    if mode == "watcher" {
        watch(&cfg).unwrap();
        return;
    }
    let id = std::env::args()
        .skip_while(|s| s != "_exec")
        .nth(1)
        .unwrap();
    let job = Job::new(&cfg, &id);
    if mode == "old-running" {
        let file = File::create(cfg.jobs.join(".slots/1")).unwrap();
        file.lock().unwrap();
        fs::write(job.turn("1", "slot"), "1\n").unwrap();
        fs::write(job.path("running"), "").unwrap();
        while !job.path("refuse").exists() {
            sleep(Duration::from_millis(10));
        }
        usage::record(
            &cfg,
            &id,
            Some(crate::now() + 3600),
            "refused",
            crate::now(),
        )
        .unwrap();
        fs::remove_file(job.turn("1", "slot")).unwrap();
        drop(file);
    }
    if mode == "new-retry" {
        Checkpoint::retry(&job, "1", false, now_nanos(), "refused").unwrap();
    }
    let held = if mode == "old-codex" {
        legacy_slot::take_engine(
            &cfg,
            &id,
            "1",
            legacy_pick::Class::Normal,
            false,
            engine::Engine::Codex,
        )
        .unwrap()
    } else if mode.starts_with("old") {
        legacy_slot::take(&cfg, &id, "1", legacy_pick::Class::Normal, false).unwrap()
    } else {
        let state = Checkpoint::load(&job, "1").unwrap();
        let engine = read(&job.turn("1", "engine"))
            .and_then(|s| engine::Engine::parse(&s).ok())
            .unwrap_or_default();
        let held = slot::take_engine(&cfg, &id, "1", Class::Normal, true, engine).unwrap();
        if let Some(state) = state {
            state.resume(&job, "1", &job.path("prompt.1")).unwrap();
        }
        held
    };
    fs::write(job.path("took"), "").unwrap();
    loop {
        std::hint::black_box(&held);
        sleep(Duration::from_secs(1));
    }
}

#[test]
fn preexisting_old_retries_are_fenced_and_transferred_without_losing_the_turn() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    for i in 0..5 {
        start(&mut r, &format!("w{i}"), "old", "/ordinary");
        until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == i + 1);
    }
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 5);
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    let stream = fs::read(r.cfg.jobs.join("w0/1.jsonl")).unwrap();
    let queued: Vec<_> = fs::read_dir(r.cfg.jobs.join(".queue"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    pass(&r.cfg).unwrap();
    usage::back(&r.cfg, None);
    until(|| took(&r.cfg) == 2);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 2);
    assert_eq!(fs::read(r.cfg.jobs.join("w0/1.jsonl")).unwrap(), stream);
    assert_eq!(
        read(&r.cfg.jobs.join("w0/prompt.1")).unwrap(),
        "the original prompt"
    );
    assert_eq!(
        read(&r.cfg.jobs.join("w0/session")).unwrap(),
        "conversation-123"
    );
    assert_eq!(read(&r.cfg.jobs.join("w0/turns")).unwrap(), "1");
    assert!(
        read(&r.cfg.jobs.join("w0/1.spent"))
            .unwrap()
            .contains("the original prompt")
    );
    assert!(
        queued
            .iter()
            .any(|n| r.cfg.jobs.join(".queue").join(n).exists())
    );
}

fn took(cfg: &Config) -> usize {
    fs::read_dir(&cfg.jobs)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.path().join("took").exists())
        .count()
}

#[test]
fn post_deploy_refusal_cannot_enter_the_old_scheduler() {
    let mut r = rig();
    start(&mut r, "running", "old-running", "/ordinary");
    until(|| r.cfg.jobs.join("running/running").exists());
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    pass(&r.cfg).unwrap();
    assert!(
        !Job::new(&r.cfg, "running")
            .turn("1", "retry-migration")
            .exists()
    );
    fs::write(r.cfg.jobs.join("running/refuse"), "").unwrap();
    until(|| !r.cfg.jobs.join("running/1.slot").exists());
    for i in 0..4 {
        start(&mut r, &format!("w{i}"), "old", "/ordinary");
    }
    pass(&r.cfg).unwrap();
    usage::back(&r.cfg, None);
    until(|| took(&r.cfg) == 2);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 2);
}

#[test]
fn unmodified_legacy_retry_loop_reproduces_the_five_turn_reset_burst() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    for i in 0..5 {
        start(&mut r, &format!("w{i}"), "old", "/ordinary");
    }
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 5);
    usage::back(&r.cfg, None);
    until(|| took(&r.cfg) == 5);
    assert_eq!(r.cfg.usage_trickle, 2);
    assert_eq!(
        took(&r.cfg),
        5,
        "the actual old scheduler ignores the reset cap"
    );
}

#[test]
fn transfer_checkpoint_survives_failed_launch_and_preserves_paused_deadline() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    start(&mut r, "retry", "old", "/ordinary");
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 1);
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    let launcher = fs::read(&r.cfg.tools.systemd_run).unwrap();
    burrow_test::executable(&r.cfg.tools.systemd_run, "#!/bin/sh\nexit 1\n");
    assert!(pass(&r.cfg).is_err());
    let job = Job::new(&r.cfg, "retry");
    let state = Checkpoint::load(&job, "1").unwrap().unwrap();
    let original_deadline = state.deadline.unwrap();
    // Restarting the consumer resumes the committed transfer; it never
    // creates another turn or replaces the conversation or original stream.
    burrow_test::executable(&r.cfg.tools.systemd_run, launcher);
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    sleep(Duration::from_secs(2));
    pass(&r.cfg).unwrap();
    sleep(Duration::from_millis(100));
    usage::back(&r.cfg, None);
    until(|| job.path("took").exists());
    let deadline: i64 = read(&job.turn("1", "deadline")).unwrap().parse().unwrap();
    assert!(deadline >= original_deadline + 100_000_000);
    assert!(deadline <= original_deadline + now_nanos() - state.waited);
    assert_eq!(read(&job.path("turns")).as_deref(), Some("1"));
    assert!(!job.turn("1", "retry-migration").exists());
}

#[test]
fn existing_holders_count_toward_the_transferred_retry_cap() {
    let mut r = rig();
    let holder = File::create(r.cfg.jobs.join(".slots/1")).unwrap();
    holder.lock().unwrap();
    fs::write(r.cfg.jobs.join(".slots/1.who"), "normal\t/existing\n").unwrap();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    for i in 0..4 {
        start(&mut r, &format!("w{i}"), "old", "/ordinary");
    }
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 4);
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    pass(&r.cfg).unwrap();
    usage::back(&r.cfg, None);
    until(|| took(&r.cfg) == 1);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 1);
    drop(holder);
    until(|| took(&r.cfg) == 2);
}

#[test]
fn transferred_pipeline_retries_keep_both_reserved_shares_ahead_of_lead_backlog() {
    let mut r = rig();
    for (name, repo) in [("lead", "burrow"), ("pipe", "box")] {
        let dir = r.tmp.path().join(name);
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .arg(&dir)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args([
                    "remote",
                    "add",
                    "origin",
                    &format!("https://github.com/sam/{repo}")
                ])
                .status()
                .unwrap()
                .success()
        );
    }
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    for (id, name) in [
        ("lead0", "lead"),
        ("lead1", "lead"),
        ("pipe0", "pipe"),
        ("pipe1", "pipe"),
        ("ordinary0", "web"),
        ("ordinary1", "web"),
    ] {
        let path = r.tmp.path().join(name).to_string_lossy().into_owned();
        start(&mut r, id, "old", &path);
        if name != "web" {
            fs::write(r.cfg.jobs.join(id).join("pipeline"), "").unwrap();
        }
    }
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 6);
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    pass(&r.cfg).unwrap();
    usage::back(&r.cfg, None);
    until(|| took(&r.cfg) == 2);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 2);
    assert!(!r.cfg.jobs.join("lead0/took").exists());
    assert!(!r.cfg.jobs.join("lead1/took").exists());
    assert!((0..2).any(|i| r.cfg.jobs.join(format!("pipe{i}/took")).exists()));
    assert!((0..2).any(|i| r.cfg.jobs.join(format!("ordinary{i}/took")).exists()));
}

#[test]
fn deployment_barrier_leaves_codex_and_emergency_pools_independent() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    for (id, mode) in [
        ("c0", "old-codex"),
        ("c1", "old-codex"),
        ("emergency", "old-unstick"),
        ("w0", "old"),
        ("w1", "old"),
        ("w2", "old"),
    ] {
        start(&mut r, id, mode, "/ordinary");
    }
    until(|| r.cfg.jobs.join("c0/took").exists() && r.cfg.jobs.join("c1/took").exists());
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() >= 3);
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    pass(&r.cfg).unwrap();
    usage::back(&r.cfg, None);
    until(|| took(&r.cfg) == 5);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 5, "two ordinary, two Codex, one emergency");
    assert!(r.cfg.jobs.join("emergency/took").exists());
    assert!(
        !Job::new(&r.cfg, "emergency")
            .turn("1", "retry-migration")
            .exists()
    );
}

#[test]
fn filesystem_events_transfer_a_refusal_after_the_initial_deploy_scan() {
    let mut r = rig();
    start(&mut r, "running", "old-running", "/ordinary");
    until(|| r.cfg.jobs.join("running/running").exists());
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    let consumer = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "retry::tests::child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(MODE, "watcher")
        .env(ROOT, r.tmp.path())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    r.children.push(consumer);
    fs::write(r.cfg.jobs.join("running/refuse"), "").unwrap();
    until(|| {
        Job::new(&r.cfg, "running")
            .turn("1", "retry-migration")
            .exists()
    });
    usage::back(&r.cfg, None);
    until(|| r.cfg.jobs.join("running/took").exists());
    assert_eq!(
        read(&r.cfg.jobs.join("running/session")).as_deref(),
        Some("conversation-123")
    );
}

#[test]
fn transferred_turn_pins_its_conversation_engine_and_rejects_queue_path_escape() {
    let mut r = rig();
    r.cfg.roles = r.tmp.path().join("roles");
    fs::write(&r.cfg.roles, "work codex\n").unwrap();
    let job = Job::new(&r.cfg, "queued");
    fs::create_dir(&job.dir).unwrap();
    fs::write(job.path("role"), "work\n").unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.turn("1", "engine"), "claude\n").unwrap();
    assert_eq!(
        engine::choice(&r.cfg, &job).unwrap().engine,
        engine::Engine::Codex
    );
    Identity::capture(&job, "1", engine::choice(&r.cfg, &job).unwrap()).unwrap();
    fs::write(job.path("engine"), "claude").unwrap();
    let checkpoint = Checkpoint {
        waited: now_nanos(),
        said: Some("refused".into()),
        deadline: Some(now_nanos() + 1000),
        name: "2-100-queued".into(),
        first: false,
        retained: Some(true),
    };
    Identity::capture(&job, "1", engine::choice(&r.cfg, &job).unwrap()).unwrap();
    swap(
        &job.turn("1", "retry-migration"),
        &serde_json::to_string(&checkpoint).unwrap(),
    )
    .unwrap();
    assert_eq!(
        engine::choice(&r.cfg, &job).unwrap().engine,
        engine::Engine::Codex
    );
    let invalid = Checkpoint {
        name: "2-100-../../outside".into(),
        ..checkpoint
    };
    Identity::capture(&job, "1", engine::choice(&r.cfg, &job).unwrap()).unwrap();
    swap(
        &job.turn("1", "retry-migration"),
        &serde_json::to_string(&invalid).unwrap(),
    )
    .unwrap();
    assert_eq!(
        Checkpoint::load(&job, "1").unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn failed_checkpoint_write_keeps_the_old_turn_alive_behind_the_barrier() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    start(&mut r, "retry", "old", "/ordinary");
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 1);
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    let job = Job::new(&r.cfg, "retry");
    fs::create_dir(job.turn("1", "retry-migration.new")).unwrap();
    assert!(pass(&r.cfg).is_err());
    assert!(systemd::active(&r.cfg, &job.unit()));
    assert!(Checkpoint::load(&job, "1").unwrap().is_none());
    fs::remove_dir(job.turn("1", "retry-migration.new")).unwrap();
    pass(&r.cfg).unwrap();
    usage::back(&r.cfg, None);
    until(|| job.path("took").exists());
}

#[test]
fn transferred_retries_trickle_at_natural_expiry_too() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    for i in 0..5 {
        start(&mut r, &format!("w{i}"), "old", "/ordinary");
    }
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 5);
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    pass(&r.cfg).unwrap();
    let path = r.cfg.jobs.join(usage::FILE);
    let mut spent: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    spent["until"] = (crate::now() + 2).into();
    fs::write(path, serde_json::to_vec(&spent).unwrap()).unwrap();
    until(|| took(&r.cfg) == 2);
    sleep(Duration::from_secs(3));
    assert_eq!(took(&r.cfg), 2);
}

#[test]
fn new_retry_publishes_one_saved_place_under_the_usage_transaction_lock() {
    let mut r = rig();
    usage::record(
        &r.cfg,
        "limit",
        Some(crate::now() + 3600),
        "refused",
        crate::now(),
    )
    .unwrap();
    let transaction = usage::lock(&r.cfg).unwrap();
    start(&mut r, "retry", "new-retry", "/ordinary");
    let job = Job::new(&r.cfg, "retry");
    until(|| job.turn("1", "retry-migration").exists());
    let state = Checkpoint::load(&job, "1").unwrap().unwrap();
    sleep(Duration::from_millis(100));
    assert_eq!(fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count(), 0);
    drop(transaction);
    until(|| fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count() == 1);
    assert_eq!(
        crate::mtime_nanos(&r.cfg.jobs.join(".queue").join(&state.name)),
        Some(state.waited)
    );
    usage::back(&r.cfg, None);
    until(|| job.path("took").exists());
    assert_eq!(fs::read_dir(r.cfg.jobs.join(".queue")).unwrap().count(), 0);
}

#[test]
fn retained_handover_rejects_rollback_to_startup_that_would_truncate_its_stream() {
    let dir = TempDir::new().unwrap();
    let old = dir.path().join("old");
    burrow_test::executable(
        &old,
        "#!/bin/sh\n[ \"$1\" = _handover_format ] || exit 2\necho 1\n",
    );
    assert!(!slot::retry_compatible(&old));
    burrow_test::executable(
        &old,
        "#!/bin/sh\n[ \"$1\" = _retry_format ] || exit 2\necho 1\n",
    );
    assert!(!slot::retry_compatible(&old));
    burrow_test::executable(
        &old,
        "#!/bin/sh\n[ \"$1\" = _retry_format ] || exit 2\necho 2\n",
    );
    assert!(slot::retry_compatible(&old));
}

#[test]
fn failed_resume_write_retains_checkpoint_and_does_not_double_extend_the_deadline() {
    let r = rig();
    let job = Job::new(&r.cfg, "retry");
    fs::create_dir(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("prompt.1"), "original prompt\n").unwrap();
    let state = Checkpoint {
        waited: now_nanos() - 2_000_000_000,
        said: Some("refused".into()),
        deadline: Some(now_nanos() + 10_000_000_000),
        name: "2-100-retry".into(),
        first: false,
        retained: Some(true),
    };
    Identity::capture(&job, "1", engine::choice(&r.cfg, &job).unwrap()).unwrap();
    swap(
        &job.turn("1", "retry-migration"),
        &serde_json::to_string(&state).unwrap(),
    )
    .unwrap();
    fs::create_dir(job.turn("1", "spent")).unwrap();
    assert!(state.resume(&job, "1", &job.path("prompt.1")).is_err());
    assert!(Checkpoint::load(&job, "1").unwrap().is_some());
    crate::cmd::finish(&r.cfg, "retry").unwrap();
    assert!(job.turn("1", "retry-wake").exists());
    assert!(!job.turn("1", "ended").exists());
    fs::remove_dir(job.turn("1", "spent")).unwrap();
    state.resume(&job, "1", &job.path("prompt.1")).unwrap();
    let deadline: i64 = read(&job.turn("1", "deadline")).unwrap().parse().unwrap();
    assert!(deadline <= state.deadline.unwrap() + now_nanos() - state.waited);
    assert!(!job.turn("1", "retry-migration").exists());
}

#[test]
fn waiting_codex_turn_transfers_back_to_its_own_pool() {
    let mut r = rig();
    fs::create_dir_all(r.cfg.jobs.join(".codex-slots")).unwrap();
    let held: Vec<_> = (1..=2)
        .map(|i| {
            let f = File::create(r.cfg.jobs.join(format!(".codex-slots/{i}"))).unwrap();
            f.lock().unwrap();
            f
        })
        .collect();
    start(&mut r, "codex", "old-codex", "/ordinary");
    until(|| r.cfg.jobs.join("codex/1.withdrawable").exists());
    let barrier = File::create(r.cfg.jobs.join(".slots/.lock")).unwrap();
    barrier.lock().unwrap();
    pass(&r.cfg).unwrap();
    assert!(
        Job::new(&r.cfg, "codex")
            .turn("1", "retry-migration")
            .exists()
    );
    drop(held);
    until(|| r.cfg.jobs.join("codex/took").exists());
    assert!(
        read(&r.cfg.jobs.join("codex/1.slot"))
            .unwrap()
            .starts_with("codex-")
    );
    assert!(!r.cfg.jobs.join(".slots/1").exists());
}

mod review;

fn pending(r: &Rig, id: &str, retained: bool) -> (Checkpoint, String) {
    let job = Job::new(&r.cfg, id);
    fs::create_dir_all(&job.dir).unwrap();
    job.set_turns(1).unwrap();
    fs::write(job.path("prompt.1"), "the prompt\n").unwrap();
    let name = format!("2-100-{id}");
    let state = Checkpoint {
        waited: now_nanos(),
        said: retained.then(|| "refused".into()),
        deadline: retained.then(|| now_nanos() + 10_000_000_000),
        name: name.clone(),
        first: false,
        retained: Some(retained),
    };
    Identity::capture(&job, "1", engine::choice(&r.cfg, &job).unwrap()).unwrap();
    swap(
        &job.turn("1", "retry-migration"),
        &serde_json::to_string(&state).unwrap(),
    )
    .unwrap();
    (state, name)
}

#[test]
fn pending_transfer_is_unfinished_work_until_stopped() {
    let r = rig();
    let job = Job::new(&r.cfg, "p");
    pending(&r, "p", true);
    // Unit inactive, outcome absent or left by a failed launch: not finished.
    assert_eq!(job.state(), "queued");
    fs::write(job.turn("1", "retry-startup-failed"), "").unwrap();
    fs::write(job.turn("1", "outcome"), "failed\n").unwrap();
    assert_eq!(job.state(), "queued");
    assert_eq!(job.ended("1"), None);
    assert_eq!(job.lifecycle().unwrap().0, "queued");
    fs::write(job.turn("1", "stopped"), "").unwrap();
    assert_eq!(job.state(), "failed");
}

#[test]
fn pending_transfer_keeps_its_place_in_the_line() {
    let r = rig();
    let (_, name) = pending(&r, "p", true);
    // Its old process is gone (no such pid).
    fs::write(r.cfg.jobs.join(".queue").join(&name), "99999999\n").unwrap();
    assert!(!slot::in_line(&r.cfg).is_empty());
    let job = Job::new(&r.cfg, "p");
    fs::write(job.turn("1", "stopped"), "").unwrap();
    assert!(slot::in_line(&r.cfg).is_empty());
}

#[test]
fn busy_job_lock_is_reported_for_retry_and_a_stopped_checkpoint_is_not_launched() {
    let r = rig();
    let job = Job::new(&r.cfg, "p");
    pending(&r, "p", true);
    let held = job.lock().unwrap();
    assert_eq!(pass(&r.cfg).unwrap_err().kind(), io::ErrorKind::WouldBlock);
    drop(held);
    fs::write(job.turn("1", "stopped"), "").unwrap();
    assert!(pass(&r.cfg).is_ok());
    assert!(!job.turn("1", "retry-migration").exists());
    assert!(!job.path("took").exists());
}

#[test]
fn an_initial_waiter_keeps_no_deadline_but_a_retained_retry_does() {
    let r = rig();
    let job = Job::new(&r.cfg, "p");
    let (initial, _) = pending(&r, "p", false);
    assert!(initial.initial());
    assert!(initial.deadline.is_none());
    fs::write(job.turn("1", "deadline"), "5\n").unwrap();
    initial.resume(&job, "1", &job.path("prompt.1")).unwrap();
    assert_eq!(read(&job.turn("1", "deadline")).as_deref(), Some("5"));
    let (kept, _) = pending(&r, "p", true);
    let dl = kept.deadline.unwrap();
    kept.resume(&job, "1", &job.path("prompt.1")).unwrap();
    let moved: i64 = read(&job.turn("1", "deadline")).unwrap().parse().unwrap();
    assert!(moved >= dl);
}

mod review2;
#[cfg(test)]
mod review3;
