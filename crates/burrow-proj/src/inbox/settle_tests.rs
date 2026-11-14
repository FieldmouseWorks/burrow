//! A sorting job's end with GitHub refusing every call, as a spent quota
//! does: its verdict is kept and its slot freed by its end alone, the next
//! sorting job waiting for that slot is let in, and the verdict's GitHub
//! steps run once GitHub answers, once.

use std::fs;
use std::path::PathBuf;

use serde_json::{Value, json};

use super::settle::settled;
use super::sorting_tests::{Box, issue, on_github, sorting};
use crate::job::Job;
use crate::snapshot::SlotKind;

/// One sorting slot; systemctl says s1's (or s3's) end unit is still running
/// while `s1-ending` (`s3-ending`) is there, as it is inside that unit's own
/// `proj _ended`, and s2's turn is running once systemd-run has started it.
/// While `pause` is there, a look at s1's turn unit waits for it to go,
/// saying so in `paused`.
fn machine() -> Box {
    let mut b = Box::new();
    let dir = b.path("");
    let dir = dir.display();
    burrow_test::executable(
        &b.cfg.tools.systemctl,
        format!(
            r#"#!/bin/sh
for a in "$@"; do unit=$a; done
if [ "$unit" = proj-s1.service ] && [ -e '{dir}/pause' ]; then
  touch '{dir}/paused'
  while [ -e '{dir}/pause' ]; do sleep 0.05; done
fi
active=no
case "$unit" in
  proj-ended@s1.service) [ -e '{dir}/s1-ending' ] && active=yes ;;
  proj-ended@s3.service) [ -e '{dir}/s3-ending' ] && active=yes ;;
  proj-s2.service) [ -e '{dir}/s2-running' ] && active=yes ;;
esac
case "$*" in
  *is-active*) [ $active = yes ] && exit 0; exit 3 ;;
  *ActiveState*) if [ $active = yes ]; then echo active; else echo inactive; fi ;;
  *LoadState*) echo not-found ;;
esac
exit 0
"#
        ),
    );
    b.cfg.tools.systemd_run = b.path("systemd-run");
    burrow_test::executable(
        &b.cfg.tools.systemd_run,
        format!("#!/bin/sh\ntouch '{dir}/s2-running'\n"),
    );
    b.cfg.kick.clear();
    b.cfg.notify = None;
    b.cfg
        .inbox
        .as_mut()
        .unwrap()
        .triage
        .as_mut()
        .unwrap()
        .at_once = 1;
    b
}

/// gh as it is with the quota spent: every call refused, each one logged.
fn quota_spent(b: &Box) -> PathBuf {
    let gh = b.path("gh-spent");
    burrow_test::executable(
        &gh,
        format!(
            "#!/bin/sh\ncat >/dev/null\necho \"$*\" >>'{}'\nprintf 'HTTP/2.0 403 Forbidden\\nx-ratelimit-remaining: 0\\n\\n{{\"message\":\"API rate limit exceeded\"}}'\nexit 1\n",
            b.path("refused").display()
        ),
    );
    gh
}

fn issue_on_github(b: &Box, n: u64) -> Value {
    let mut i = issue(n, "sam", &[]);
    i["repository_url"] = json!("https://api.github.com/repos/sam/box");
    on_github(b, &i);
    i
}

/// Issue 8's sorting job s2, queued behind s1 for the one slot.
fn queued_behind(b: &Box) {
    b.job("s2", "done", "Which one?");
    let d = b
        .cfg
        .inbox
        .as_ref()
        .unwrap()
        .state
        .join(".triage/sam_box-8");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("job"), "s2\n").unwrap();
    let job = Job::new(&b.cfg, "s2");
    let held = job.lock().unwrap();
    crate::turn::start(&b.cfg, "s2", "A person answered.", false).unwrap();
    drop(held);
    assert!(crate::policy::parked(&job), "s1 holds the only slot");
    assert!(!b.path("s2-running").exists());
}

/// The sorting slots as the pass counts them and as `proj state` shows them:
/// the issues holding one, each way.
fn slots(b: &Box) -> (Vec<String>, Vec<String>) {
    let p = b.pass();
    let mut counted: Vec<String> = super::dirs(&p.sorting_dir(), "")
        .into_iter()
        .filter(|d| p.sorting_slot(d))
        .map(|d| {
            let name = d.file_name().unwrap().to_string_lossy().into_owned();
            let (repo, n) = name.rsplit_once('-').unwrap();
            format!("{}#{n}", repo.replacen('_', "/", 1))
        })
        .collect();
    counted.sort();
    let mut shown: Vec<String> = super::view::view(&b.cfg, 0)
        .slots
        .into_iter()
        .filter(|s| s.kind == SlotKind::Sorting)
        .filter_map(|s| s.holder.and_then(|h| h.issue))
        .collect();
    shown.sort();
    (counted, shown)
}

#[test]
fn a_sorting_jobs_end_frees_its_slot_and_keeps_its_verdict_while_github_refuses() {
    let mut b = machine();
    let d = sorting(&b, "ready high\nA pipeline blocker.");
    fs::write(b.cfg.jobs.join("s1/1.engine"), "codex").unwrap();
    // s1's turn is over and its end unit is running: its `proj _ended`.
    fs::write(b.path("s1-ending"), "").unwrap();
    queued_behind(&b);
    let i7 = issue_on_github(&b, 7);
    let answers = b.cfg.tools.gh.clone();
    b.cfg.tools.gh = quota_spent(&b);

    crate::cmd::finish(&b.cfg, "s1").unwrap();
    assert!(
        !b.path("refused").exists(),
        "the end asks GitHub nothing: {}",
        fs::read_to_string(b.path("refused")).unwrap_or_default()
    );
    let kept = settled(&Job::new(&b.cfg, "s1")).expect("its verdict is kept");
    assert!(
        kept.answer.as_deref().unwrap().starts_with("ready high"),
        "{kept:?}"
    );
    // proj's queue, woken while s1's end unit is still running, lets s2 in.
    crate::policy::reconcile(&b.cfg).unwrap();
    assert!(b.path("s2-running").exists(), "s2 took the freed slot");
    assert!(!crate::policy::parked(&Job::new(&b.cfg, "s2")));
    // The pass and `proj state` see the same one slot, held by s2.
    let (counted, shown) = slots(&b);
    assert_eq!(counted, ["sam/box#8"]);
    assert_eq!(shown, counted);

    // The verdict's GitHub steps wait for GitHub, read from the record: the
    // job's own stream isn't needed again.
    fs::remove_file(b.cfg.jobs.join("s1/1.jsonl")).unwrap();
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    b.pass()
        .verdicts("sam/box", &t, std::slice::from_ref(&i7))
        .unwrap();
    assert!(d.exists(), "kept until GitHub takes its steps");
    assert!(b.log().is_empty(), "{}", b.log());
    assert_eq!(slots(&b).0, ["sam/box#8"]);

    // GitHub answers: the steps run, once.
    b.cfg.tools.gh = answers;
    for _ in 0..2 {
        b.pass()
            .verdicts("sam/box", &t, std::slice::from_ref(&i7))
            .unwrap();
    }
    assert!(!d.exists(), "the record goes once it's carried out");
    let log = b.log();
    assert_eq!(log.matches("labels[]=ready").count(), 1, "{log}");
    assert_eq!(log.matches("labels[]=priority: high").count(), 1, "{log}");
}

#[test]
fn a_pass_settles_a_missed_end_before_it_asks_github() {
    let mut b = machine();
    sorting(&b, "later\nWaits on #4.");
    fs::write(b.path("s1-ending"), "").unwrap();
    b.cfg.tools.gh = quota_spent(&b);
    // Its end unit hasn't finished: its holds may not all be written yet.
    b.pass().run("sam/box").unwrap();
    assert!(settled(&Job::new(&b.cfg, "s1")).is_none());
    assert_eq!(slots(&b).0, ["sam/box#7"]);
    // It was killed before it settled anything: the next pass does, though
    // GitHub refuses that pass everything.
    fs::remove_file(b.path("s1-ending")).unwrap();
    b.pass().run("sam/box").unwrap();
    let job = Job::new(&b.cfg, "s1");
    let kept = settled(&job).expect("settled by the pass");
    assert!(kept.answer.unwrap().starts_with("later"));
    assert_eq!(slots(&b), (vec![], vec![]));
    // A later turn's answer isn't the kept one's.
    job.set_turns(2).unwrap();
    assert!(settled(&job).is_none());
}

/// Run `f` on another thread: whether it finished within ten seconds.
fn finishes(f: impl FnOnce() + Send + 'static) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx.recv_timeout(std::time::Duration::from_secs(10)).is_ok()
}

// A stop finishing the turn (a retained retry's cancellation) holds the job's
// lock, as does a reply waiting out this end unit: the end never waits for
// it, and leaves the settling to the next pass.
#[test]
fn an_end_never_waits_for_the_jobs_lock() {
    let b = machine();
    sorting(&b, "ready high\nA pipeline blocker.");
    fs::write(b.path("s1-ending"), "").unwrap();
    let job = Job::new(&b.cfg, "s1");
    let held = job.lock().unwrap();
    let cfg = b.cfg.clone();
    assert!(
        finishes(move || super::settle::ended(&cfg, "s1")),
        "the end waited for the job's lock"
    );
    assert!(settled(&job).is_none());
    drop(held);
    assert_eq!(slots(&b).0, ["sam/box#7"]);
    // Once its end unit has stopped, the next pass settles it.
    fs::remove_file(b.path("s1-ending")).unwrap();
    b.pass().settle();
    assert!(settled(&job).is_some());
    assert_eq!(slots(&b).0, Vec::<String>::new());
}

// A reply's turn is published before it's queued or launched: a turn with no
// end of its own on record is never settled, so its coming answer isn't
// dropped as none, nor its slot given up once it runs.
#[test]
fn a_turn_being_started_is_never_settled() {
    let b = machine();
    sorting(&b, "ready high\nA pipeline blocker.");
    let job = Job::new(&b.cfg, "s1");
    fs::write(job.path("prompt.2"), "A person answered.").unwrap();
    job.set_turns(2).unwrap();
    b.pass().settle();
    super::settle::ended(&b.cfg, "s1");
    assert!(settled(&job).is_none(), "settled a turn that hasn't run");
    // The turn runs and answers: that answer is the one kept.
    fs::write(job.turn("2", "outcome"), "done\n").unwrap();
    fs::write(job.turn("2", "end"), "done\n").unwrap();
    fs::write(
        job.turn("2", "jsonl"),
        format!(
            "{}\n",
            json!({"type": "result", "result": "later\nWaits on #4."})
        ),
    )
    .unwrap();
    b.pass().settle();
    let kept = settled(&job).expect("settled once it ended");
    assert_eq!(kept.turn, "2");
    assert!(kept.answer.unwrap().starts_with("later"));
}

// The issue is sorted again while an old job's end is still settling: the
// record now names the new job s3, whose own end hasn't run. What the old end
// keeps is its own job's, so s3 keeps its slot and its own verdict.
#[test]
fn a_late_end_never_settles_the_job_that_replaced_it() {
    let b = machine();
    let d = sorting(&b, "ready high\nA pipeline blocker.");
    b.job("s3", "done", "later\nWaits on #4.");
    fs::write(b.path("s3-ending"), "").unwrap();
    fs::write(b.path("pause"), "").unwrap();
    let cfg = b.cfg.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        super::settle::ended(&cfg, "s1");
        let _ = tx.send(());
    });
    let paused = b.path("paused");
    let start = std::time::Instant::now();
    while !paused.exists() {
        assert!(start.elapsed().as_secs() < 10, "the end never looked at s1");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    fs::remove_dir_all(&d).unwrap();
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("job"), "s3\n").unwrap();
    fs::remove_file(b.path("pause")).unwrap();
    rx.recv_timeout(std::time::Duration::from_secs(10)).unwrap();
    assert!(settled(&Job::new(&b.cfg, "s3")).is_none());
    assert_eq!(slots(&b).0, ["sam/box#7"]);
    assert!(settled(&Job::new(&b.cfg, "s1")).is_some());
}

/// Policy releases s1's parked turn `turn`, from `state`: its restoration is
/// prepared, and not yet accepted by an `_exec`.
fn restoring(job: &Job, state: &str, turn: &str) {
    let rec = crate::policy::Park {
        state: state.into(),
        turn: turn.into(),
        first: false,
        at: 0,
        wait: None,
    };
    fs::write(
        crate::policy::path(job, "restoring"),
        serde_json::to_string(&rec).unwrap(),
    )
    .unwrap();
}

/// Turn `n`'s `_exec` failed, or was killed, before it accepted its
/// restoration: its outcome and its end's mark are on record.
fn failed_before_acceptance(job: &Job, n: &str) {
    fs::write(job.turn(n, "outcome"), "failed\n").unwrap();
    fs::write(job.turn(n, "ended"), "").unwrap();
}

/// Neither the end nor a pass settles s1 now.
fn unsettled(b: &Box, job: &Job) {
    super::settle::ended(&b.cfg, "s1");
    b.pass().settle();
    assert!(settled(job).is_none(), "settled an attempt policy reruns");
}

/// Turn `n` runs again, is accepted, and answers `said`.
fn accepted_and_answers(job: &Job, n: &str, said: &str) {
    crate::policy::accepted(job).unwrap();
    let _ = fs::remove_file(job.turn(n, "ended"));
    fs::write(job.turn(n, "outcome"), "done\n").unwrap();
    fs::write(job.turn(n, "end"), "done\n").unwrap();
    fs::write(
        job.turn(n, "jsonl"),
        format!("{}\n", json!({"type": "result", "result": said})),
    )
    .unwrap();
}

// A queued turn's restoration whose `_exec` fails before it's accepted is
// relaunched by policy under the same number: that failure is never settled,
// by the end or a pass, and a settlement kept from it is void, so the
// relaunched turn keeps its slot and its own answer is the one kept.
#[test]
fn a_restoration_that_fails_before_acceptance_is_never_settled() {
    let b = machine();
    sorting(&b, "ready high\nA pipeline blocker.");
    let job = Job::new(&b.cfg, "s1");
    fs::write(job.path("prompt.2"), "A person answered.").unwrap();
    job.set_turns(2).unwrap();
    restoring(&job, "queued", "2");
    failed_before_acceptance(&job, "2");
    unsettled(&b, &job);
    // One kept anyway (by an end that read before the restoration was
    // released) is never read while the turn can run again.
    let stale = crate::ask::host_path(&job, "sorting-ended");
    fs::write(&stale, r#"{"turn":"2","answer":null}"#).unwrap();
    assert!(
        settled(&job).is_none(),
        "a kept failure outlived a relaunch"
    );
    fs::remove_file(&stale).unwrap();
    // Reconciliation parks it again to relaunch it: still unsettled.
    fs::rename(
        crate::policy::path(&job, "restoring"),
        crate::policy::path(&job, "park"),
    )
    .unwrap();
    unsettled(&b, &job);
    // Released and accepted, it answers: that answer is the one kept.
    fs::rename(
        crate::policy::path(&job, "park"),
        crate::policy::path(&job, "restoring"),
    )
    .unwrap();
    accepted_and_answers(&job, "2", "later\nWaits on #4.");
    b.pass().settle();
    let kept = settled(&job).expect("settled once the relaunch ended");
    assert_eq!(kept.turn, "2");
    assert!(
        kept.answer.unwrap().starts_with("later"),
        "kept the failure"
    );
    assert_eq!(slots(&b).0, Vec::<String>::new());
}

// A running turn's restoration starts a new turn; if its `_exec` dies before
// it's accepted, reconciliation rolls the job back to the parked turn and the
// next release reuses the new turn's number. Neither the dead attempt nor the
// rolled-back turn is settled, so the reused number's own answer is kept.
#[test]
fn a_rolled_back_restoration_never_settles_the_number_it_reuses() {
    let b = machine();
    sorting(&b, "ready high\nA pipeline blocker.");
    let job = Job::new(&b.cfg, "s1");
    restoring(&job, "running", "1");
    fs::write(job.path("prompt.2"), "Resume where you left off.").unwrap();
    job.set_turns(2).unwrap();
    failed_before_acceptance(&job, "2");
    unsettled(&b, &job);
    // Reconciliation (`policy::reconcile_job`) rolls it back to the parked
    // turn, parked again.
    job.set_turns(1).unwrap();
    fs::rename(
        crate::policy::path(&job, "restoring"),
        crate::policy::path(&job, "park"),
    )
    .unwrap();
    unsettled(&b, &job);
    // Released again, it reuses turn 2, is accepted and answers.
    fs::rename(
        crate::policy::path(&job, "park"),
        crate::policy::path(&job, "restoring"),
    )
    .unwrap();
    job.set_turns(2).unwrap();
    accepted_and_answers(&job, "2", "later\nWaits on #4.");
    super::settle::ended(&b.cfg, "s1");
    let kept = settled(&job).expect("settled once the reused turn ended");
    assert_eq!(kept.turn, "2");
    assert!(
        kept.answer.unwrap().starts_with("later"),
        "kept the dead attempt"
    );
    assert_eq!(slots(&b).0, Vec::<String>::new());
}
