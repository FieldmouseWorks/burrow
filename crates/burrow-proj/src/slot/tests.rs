// What a look at the line costs: git at most once a project, however many
// wait, and the note of who's next lets every other waiter skip its look, for
// as long as it's fresh. A look forks nothing else: slots' locks and the
// waiters' files are all read in this process, so git is all there is to count.
//
// What takes real processes runs in a child of this test binary (test
// `child`), told what to do by CHILD_ENV. Its arguments carry the _exec the
// line's liveness check looks for, past libtest, which reads them as more
// names of tests to run.

use std::env;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process};
use tempfile::TempDir;

use super::{
    LOOK_EVERY, NEXT_FRESH, came, in_line, look, look_due, note_next, place_left, successor, take,
};
use crate::config::Config;
use crate::config::tests::example;
use crate::now;
use crate::pick::{Class, Slot};

const CHILD_ENV: &str = "BURROW_PROJ_TEST_CHILD";
const JOBS_ENV: &str = "BURROW_PROJ_TEST_JOBS";
const MAX_JOBS_ENV: &str = "BURROW_PROJ_TEST_MAX_JOBS";

fn config(jobs: &Path, max_jobs: usize) -> Config {
    let mut v = example();
    v["jobs"] = jobs.to_str().unwrap().into();
    v["maxJobs"] = max_jobs.into();
    v["pipelineSlots"] = 0.into();
    v["maxJobsPerProject"] = 4.into();
    v["minFreeMemory"] = 0.into();
    v["maxWait"] = 99999.into();
    v["pipelineRepos"] = serde_json::json!(["sam/burrow", "sam/box"]);
    v["leadRepos"] = serde_json::json!([]);
    v["flake"] = "/nowhere".into();
    v["tools"]["git"] = "git".into();
    serde_json::from_value(v).unwrap()
}

fn jobs_dir(tmp: &TempDir) -> std::path::PathBuf {
    let jobs = tmp.path().join("jobs");
    fs::create_dir_all(jobs.join(".queue")).unwrap();
    fs::create_dir_all(jobs.join(".slots")).unwrap();
    jobs
}

fn git(args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

fn clones(tmp: &TempDir) {
    for (name, url) in [
        ("burrow", "https://github.com/sam/burrow"),
        ("box", "https://github.com/sam/box"),
        ("web", "https://github.com/sam/web"),
    ] {
        let p = tmp.path().join(name);
        git(&["init", "-q", p.to_str().unwrap()]);
        git(&["-C", p.to_str().unwrap(), "remote", "add", "origin", url]);
    }
}

/// A process that dies with the test.
struct Kid(Child);

impl Drop for Kid {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_child(what: &str, jobs: &Path, max_jobs: usize, args: &[String]) -> Kid {
    let child = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "slot::tests::child",
            "--nocapture",
            "--test-threads=1",
        ])
        .args(args)
        .env(CHILD_ENV, what)
        .env(JOBS_ENV, jobs)
        .env(MAX_JOBS_ENV, max_jobs.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Kid(child)
}

// Not a test of its own: what spawn_child asks for, run in the child, which
// stays up (its stdin never closes) so what it holds stays held. Run plain, it
// does nothing.
#[test]
fn child() {
    let Ok(what) = env::var(CHILD_ENV) else {
        return;
    };
    let jobs = env::var(JOBS_ENV).unwrap();
    let max_jobs: usize = env::var(MAX_JOBS_ENV).unwrap().parse().unwrap();
    let cfg = config(Path::new(&jobs), max_jobs);
    let mut held = None;
    if what == "take" || what == "leave" {
        let id = env::args().skip_while(|a| a != "_exec").nth(1).unwrap();
        held = Some(take(&cfg, &id, "1", Class::Normal).unwrap());
        // Work the turn leaves behind: a sleep in the background of a shell
        // that's gone, so nothing's child, which ignores TERM.
        if what == "leave" {
            let left = cfg.jobs.join(&id).join("left");
            assert!(
                Command::new("sh")
                    .args([
                        "-c",
                        r#"trap '' TERM; sleep 600 </dev/null >/dev/null 2>&1 & echo $! >"$0""#
                    ])
                    .arg(&left)
                    .stdin(Stdio::null())
                    .status()
                    .unwrap()
                    .success()
            );
        }
        fs::write(cfg.jobs.join(&id).join("took"), now().to_string()).unwrap();
    }
    let _ = std::io::stdin().read(&mut [0]);
    drop(held);
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{what}, after 30s"
        );
        sleep(Duration::from_millis(20));
    }
}

fn in_queue(jobs: &Path) -> usize {
    fs::read_dir(jobs.join(".queue")).unwrap().count()
}

/// One process stands in for N waiters, all --pipeline, across three projects:
/// its command line has each one's _exec.
fn line_of_size(tmp: &TempDir, jobs: &Path, n: usize) -> Kid {
    let mut args = Vec::new();
    for i in 0..n {
        let j = format!("job{i}");
        args.extend(["_exec".to_owned(), j.clone(), "1".into(), "normal".into()]);
        fs::create_dir_all(jobs.join(&j)).unwrap();
        let p = ["burrow", "box", "web"][i % 3];
        let project = tmp.path().join(p);
        fs::write(
            jobs.join(&j).join("project"),
            format!("{}\n", project.display()),
        )
        .unwrap();
        File::create(jobs.join(&j).join("pipeline")).unwrap();
    }
    let kid = spawn_child("hold", jobs, 1, &args);
    let pid = kid.0.id();
    until("the holder's command line", || {
        fs::read(format!("/proc/{pid}/cmdline")).is_ok_and(|c| c.windows(5).any(|w| w == b"_exec"))
    });
    for i in 0..n {
        fs::write(
            jobs.join(".queue").join(format!("2-{}-job{i}", 1000 + i)),
            format!("{pid}\n"),
        )
        .unwrap();
    }
    kid
}

// The stand-in git is written and run where nothing else forks: a child forked
// elsewhere in this process while it's open to write would fail its exec.
#[test]
fn a_look_runs_git_once_a_project() {
    let tmp = TempDir::new().unwrap();
    let status = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "slot::tests::counted_look",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("BURROW_PROJ_TEST_COUNT", tmp.path())
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn counted_look() {
    let Ok(dir) = env::var("BURROW_PROJ_TEST_COUNT") else {
        return;
    };
    let tmp = TempDir::new_in(dir).unwrap();
    let jobs = jobs_dir(&tmp);
    clones(&tmp);
    let log = tmp.path().join("git.log");
    let stub = tmp.path().join("git");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho \"$*\" >>{}\nexec git \"$@\"\n",
            log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = config(&jobs, 6);
    cfg.tools.git = stub;
    let _holder = line_of_size(&tmp, &jobs, 60);
    let seen = look(&cfg, "none", &jobs.join("none"), Class::Normal).unwrap();
    let runs = fs::read_to_string(&log).unwrap().lines().count();
    assert_eq!(runs, 3, "a look's git runs for 3 projects");
    assert_eq!(seen.line.len(), 60);
    assert_eq!(seen.line[0].class, Class::Pipeline, "job0, in burrow");
    assert_eq!(seen.line[2].class, Class::First, "job2, in web");
    assert!(seen.line[0].project.ends_with("/burrow"));
}

// Turns that are gone lose their place; a pid that's another's does too.
#[test]
fn a_look_clears_dead_places() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    clones(&tmp);
    let cfg = config(&jobs, 6);
    let _holder = line_of_size(&tmp, &jobs, 2);
    let queue = jobs.join(".queue");
    fs::write(
        queue.join("2-3000-job9"),
        format!("{}\n", std::process::id()),
    )
    .unwrap();
    fs::write(queue.join("2-3001-job8"), "999999999\n").unwrap();
    fs::write(queue.join("2-3002-job7"), "").unwrap();
    let seen = look(&cfg, "none", &jobs.join("none"), Class::Normal).unwrap();
    assert_eq!(seen.line.len(), 2);
    assert_eq!(in_queue(&jobs), 2, "dead places stayed");
}

// A taken slot reads as its N.who, or as not known; a free one as free.
#[test]
fn a_look_reads_the_slots() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 6);
    let slots = jobs.join(".slots");
    let (a, b) = (
        File::create(slots.join("1")).unwrap(),
        File::create(slots.join("2")).unwrap(),
    );
    a.try_lock().unwrap();
    b.try_lock().unwrap();
    fs::write(slots.join("1.who"), "normal\t/src/my web\n").unwrap();
    let seen = look(&cfg, "none", &jobs.join("none"), Class::Normal).unwrap();
    assert_eq!(
        seen.slots[0],
        Slot::Held {
            class: Some(Class::Normal),
            project: "/src/my web".into()
        }
    );
    assert_eq!(
        seen.slots[1],
        Slot::Held {
            class: None,
            project: "-".into()
        }
    );
    assert_eq!(seen.slots[2], Slot::Free);
    assert!(seen.busy);
    // What it found free it let go of: it's still there for the taking.
    let c = File::create(slots.join("3")).unwrap();
    c.try_lock().unwrap();
    a.unlock().unwrap();
    b.unlock().unwrap();
    c.unlock().unwrap();
    let seen = look(&cfg, "none", &jobs.join("none"), Class::Normal).unwrap();
    assert!(seen.slots.iter().all(|s| *s == Slot::Free));
    assert!(!seen.busy);
}

// Who looks: nobody else while the note is fresh, its name always, and everyone
// once it's old, torn or gone, or once they haven't looked in LOOK_EVERY.
#[test]
fn who_looks() {
    let now = 1_000_000;
    let due = |note: &str, me: &str, looked: i64| look_due(Some(note), me, looked, now);
    let note = format!("{now}\t/q/next\n");
    assert!(!due(&note, "/q/other", now), "fresh, naming another");
    assert!(due(&note, "/q/next", now), "fresh, naming it");
    assert!(due(&note, "/q/other", 0), "never looked");
    assert!(
        due(&note, "/q/other", now - LOOK_EVERY),
        "hasn't looked in LOOK_EVERY"
    );
    let two = format!("{now}\t/q/next\t/q/2-1-b\n");
    assert!(due(&two, "/q/2-1-b", now), "named second");
    assert!(due(&two, "/q/next", now), "named first");
    assert!(!due(&two, "/q/2-1", now), "one its name starts");
    assert!(!due(&two, "/q/other", now), "naming two others");
    assert!(!due(&format!("{now}\t\n"), "/q/other", now), "nobody goes");
    assert!(!due(&format!("{now}\n"), "/q/other", now), "nobody goes");
    let old = format!("{}\t/q/next\n", now - NEXT_FRESH);
    assert!(due(&old, "/q/other", now), "an old note");
    let ahead = format!("{}\t/q/next\n", now + 100);
    assert!(due(&ahead, "/q/other", now), "a note from the future");
    assert!(due("12x\n", "/q/other", now), "torn: not a time");
    assert!(
        due(&format!("{now}\t/q/next"), "/q/other", now),
        "torn: no newline yet"
    );
    assert!(due("", "/q/other", now), "an empty note");
    assert!(look_due(None, "/q/other", now, now), "no note");
}

// What pick says becomes the note, every name of it; nothing, a note of nobody.
// A bash turn still in line when a machine switches reads the same note.
#[test]
fn the_note_is_what_pick_said() {
    let tmp = TempDir::new().unwrap();
    let slots = tmp.path();
    let at = now();
    let read = || fs::read_to_string(slots.join(".next")).unwrap();
    note_next(slots, &["/q/a".into(), "/q/b".into()]).unwrap();
    assert_eq!(read(), format!("{at}\t/q/a\t/q/b\n"));
    assert!(look_due(Some(&read()), "/q/a", at, at));
    assert!(look_due(Some(&read()), "/q/b", at, at));
    assert!(!look_due(Some(&read()), "/q/c", at, at));
    note_next(slots, &[]).unwrap();
    assert!(!look_due(Some(&read()), "/q/a", at, at));
}

fn waiter(jobs: &Path, tmp: &TempDir, id: &str, max_jobs: usize) -> Kid {
    fs::create_dir_all(jobs.join(id)).unwrap();
    fs::write(
        jobs.join(id).join("project"),
        format!("{}\n", tmp.path().join("web").display()),
    )
    .unwrap();
    spawn_child(
        "take",
        jobs,
        max_jobs,
        &["_exec".into(), id.into(), "1".into(), "normal".into()],
    )
}

fn took(jobs: &Path, id: &str) -> Option<i64> {
    fs::read_to_string(jobs.join(id).join("took"))
        .ok()?
        .parse()
        .ok()
}

/// Waiters joining the line one at a time, so their order is known, all then
/// given time to have looked and found nothing free.
fn waiters(jobs: &Path, tmp: &TempDir, ids: &[&str], max_jobs: usize) -> Vec<Kid> {
    let mut kids = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        kids.push(waiter(jobs, tmp, id, max_jobs));
        until("a waiter's place", || in_queue(jobs) == i + 1);
    }
    sleep(Duration::from_secs(3));
    kids
}

// Whole, with real processes: the line's head takes the free slot within
// seconds of it coming free, with the rest of the line skipping their looks.
#[test]
fn the_head_takes_a_slot_as_it_frees() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let held = File::create(jobs.join(".slots/1")).unwrap();
    held.try_lock().unwrap();
    let _kids = waiters(&jobs, &tmp, &["w0", "w1", "w2", "w3", "w4"], 1);
    let freed = now();
    held.unlock().unwrap();
    until("the head of the line takes the slot", || {
        took(&jobs, "w0").is_some()
    });
    let at = took(&jobs, "w0").unwrap();
    assert!(at - freed <= 6, "took a free slot {}s after", at - freed);
    for w in ["w1", "w2", "w3", "w4"] {
        assert_eq!(took(&jobs, w), None, "{w} took a slot w0 holds");
    }
    until("w0's place goes", || in_queue(&jobs) == 4);
}

// Two slots free at once: both go in a look or two, the next two in line, with
// the note's skipping holding up neither.
#[test]
fn two_free_slots_both_go() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let (a, b) = (
        File::create(jobs.join(".slots/1")).unwrap(),
        File::create(jobs.join(".slots/2")).unwrap(),
    );
    a.try_lock().unwrap();
    b.try_lock().unwrap();
    let _kids = waiters(&jobs, &tmp, &["w0", "w1", "w2", "w3", "w4"], 2);
    let freed = now();
    a.unlock().unwrap();
    b.unlock().unwrap();
    for w in ["w0", "w1"] {
        until("the next two take the slots", || took(&jobs, w).is_some());
        let at = took(&jobs, w).unwrap();
        assert!(at - freed <= 6, "{w} took a slot {}s after", at - freed);
    }
    for w in ["w2", "w3", "w4"] {
        assert_eq!(took(&jobs, w), None, "{w} went ahead of the line");
    }
    until("their places go", || in_queue(&jobs) == 3);
}

// All of the line, in the order pick takes them, each with its wait, and only
// read.
#[test]
fn the_line_as_status_shows_it() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    clones(&tmp);
    let mut cfg = config(&jobs, 6);
    cfg.lead_repos = vec!["sam/burrow".into()];
    let _holder = line_of_size(&tmp, &jobs, 4);
    let got = in_line(&cfg);
    let rows: Vec<Vec<&str>> = got
        .lines()
        .map(|l| l.split_whitespace().collect())
        .collect();
    let who: Vec<(&str, &str)> = rows.iter().map(|r| (r[0], r[1])).collect();
    assert_eq!(
        who,
        [
            ("job0", "lead"),
            ("job3", "lead"),
            ("job1", "pipeline"),
            ("job2", "first")
        ]
    );
    assert_eq!(in_queue(&jobs), 4, "it changed the line");
    let wait = rows[0][2];
    let (m, s) = wait.strip_suffix('s').unwrap().split_once('m').unwrap();
    assert!(
        m.parse::<i64>().is_ok() && s.len() == 2 && s.parse::<i64>().is_ok(),
        "{wait}"
    );
    assert!(rows[0][3].ends_with("/burrow"));
    // Nobody in line, or no line at all, is nothing to show.
    let none = TempDir::new().unwrap();
    assert_eq!(in_line(&config(none.path(), 6)), "");
}

// A hand-over's place is the job's own, under this pid, and keeps when it came.
#[test]
fn a_place_left_by_a_hand_over() {
    let tmp = TempDir::new().unwrap();
    let q = tmp.path();
    fs::write(q.join("2-5000000000-web-1"), "42\n").unwrap();
    fs::write(q.join("1-6000000000-my-web-1"), "42\n").unwrap();
    fs::write(q.join("2-7000000000-web-2"), "43\n").unwrap();
    fs::write(q.join(".2-8000000000-web-3.new"), "44\n").unwrap();
    assert_eq!(
        place_left(q, "web-1", 42).as_deref(),
        Some("2-5000000000-web-1")
    );
    assert_eq!(place_left(q, "web-1", 43), None);
    assert_eq!(place_left(q, "web-3", 44), None);
    assert_eq!(place_left(q, "web-9", 42), None);
    assert_eq!(came("2-5000000000-web-1"), 5);
}

// Who's handed over to: only a proj that isn't this one and prints the same
// format. Scripts are written and run where nothing else forks, as above.
#[test]
fn who_is_handed_over_to() {
    let tmp = TempDir::new().unwrap();
    let status = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "slot::tests::successors",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("BURROW_PROJ_TEST_COUNT", tmp.path())
        .output()
        .unwrap();
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stdout)
    );
}

#[test]
fn successors() {
    let Ok(dir) = env::var("BURROW_PROJ_TEST_COUNT") else {
        return;
    };
    let dir = Path::new(&dir).canonicalize().unwrap();
    let mut cfg = config(&dir, 1);
    let script = |name: &str, body: &str| {
        let p = dir.join(name);
        fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let new = script("new", "[ \"$1\" = _handover_format ] && echo 1");
    let other = script("other", "echo 2");
    let older = script("older", "echo 'usage: proj run|reply' >&2; exit 2");
    cfg.me = dir.join("me");
    cfg.deployed = dir.join("nowhere");
    assert_eq!(successor(&cfg), None, "nothing deployed");
    cfg.deployed = new.clone();
    assert_eq!(successor(&cfg).as_ref(), Some(&new));
    cfg.me = new.clone();
    assert_eq!(successor(&cfg), None, "it's this one");
    cfg.me = dir.join("me");
    let link = dir.join("link");
    std::os::unix::fs::symlink(&new, &link).unwrap();
    cfg.deployed = link;
    assert_eq!(successor(&cfg).as_ref(), Some(&new), "its store path");
    cfg.deployed = other;
    assert_eq!(successor(&cfg), None, "another format");
    cfg.deployed = older;
    assert_eq!(successor(&cfg), None, "from before the hand-over");
}

// An --unstick turn takes its own slot, past the rest, and joins no line; only
// another holds it up.
#[test]
fn an_unstick_turn_takes_its_own_slot() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let cfg = config(&jobs, 1);
    let held = File::create(jobs.join(".slots/1")).unwrap();
    held.try_lock().unwrap();
    fs::create_dir_all(jobs.join("u")).unwrap();
    fs::write(jobs.join("u/project"), "/src/web\n").unwrap();
    fs::write(jobs.join("u/unstick"), "").unwrap();
    let slot = take(&cfg, "u", "1", Class::Normal).unwrap();
    assert_eq!(
        fs::read_to_string(jobs.join("u/1.slot")).unwrap(),
        "unstick\n"
    );
    assert_eq!(
        fs::read_to_string(jobs.join(".slots/unstick.who")).unwrap(),
        "unstick\t/src/web\n"
    );
    assert_eq!(in_queue(&jobs), 0);
    let another = File::create(jobs.join(".slots/unstick")).unwrap();
    assert!(another.try_lock().is_err(), "the slot isn't held");
    slot.unlock().unwrap();
    held.unlock().unwrap();
}

/// What a test's turn left running, killed however the test ends.
struct Left(Pid);

impl Drop for Left {
    fn drop(&mut self) {
        let _ = kill_process(self.0, Signal::KILL);
    }
}

/// Whether a slot is free, as a waiter would find it. Let go of by unlocking,
/// not by closing: a child another test forks meanwhile shares the file until
/// it execs.
fn free(slot: &Path) -> bool {
    let f = File::create(slot).unwrap();
    let ok = f.try_lock().is_ok();
    if ok {
        f.unlock().unwrap();
    }
    ok
}

// A slot stays with what its turn left running: with the turn's process
// killed and the work it left (detached, and deaf to TERM) still up, no
// waiter can take it; once that's gone, it's free. A numbered slot and the
// --unstick one alike.
#[test]
fn a_slot_stays_with_what_its_turn_left() {
    for (id, slot) in [("t", "1"), ("u", "unstick")] {
        let tmp = TempDir::new().unwrap();
        let jobs = jobs_dir(&tmp);
        fs::create_dir_all(jobs.join(id)).unwrap();
        fs::write(jobs.join(id).join("project"), "/src/web\n").unwrap();
        if slot == "unstick" {
            fs::write(jobs.join(id).join("unstick"), "").unwrap();
        }
        let turn = spawn_child(
            "leave",
            &jobs,
            1,
            &["_exec".into(), id.into(), "1".into(), "normal".into()],
        );
        until("the turn takes its slot", || took(&jobs, id).is_some());
        let pid: i32 = fs::read_to_string(jobs.join(id).join("left"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let left = Left(Pid::from_raw(pid).unwrap());
        let slot = jobs.join(".slots").join(slot);
        drop(turn);
        kill_process(left.0, Signal::TERM).unwrap();
        sleep(Duration::from_millis(500));
        assert!(
            !free(&slot),
            "{id}: the slot went with its turn while what it left still runs"
        );
        drop(left);
        until("the slot frees once what was left is gone", || free(&slot));
    }
}
