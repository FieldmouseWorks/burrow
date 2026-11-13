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

/// One sorting slot; systemctl says s1's end unit is still running while
/// `s1-ending` is there (as it is inside that unit's own `proj _ended`), and
/// s2's turn is running once systemd-run has started it.
fn machine() -> Box {
    let mut b = Box::new();
    let dir = b.path("");
    let dir = dir.display();
    burrow_test::executable(
        &b.cfg.tools.systemctl,
        format!(
            r#"#!/bin/sh
for a in "$@"; do unit=$a; done
active=no
case "$unit" in
  proj-ended@s1.service) [ -e '{dir}/s1-ending' ] && active=yes ;;
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
    let kept = settled(&d, &Job::new(&b.cfg, "s1")).expect("its verdict is kept");
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
    let d = sorting(&b, "later\nWaits on #4.");
    fs::write(b.path("s1-ending"), "").unwrap();
    b.cfg.tools.gh = quota_spent(&b);
    // Its end unit hasn't finished: its holds may not all be written yet.
    b.pass().run("sam/box").unwrap();
    assert!(settled(&d, &Job::new(&b.cfg, "s1")).is_none());
    assert_eq!(slots(&b).0, ["sam/box#7"]);
    // It was killed before it settled anything: the next pass does, though
    // GitHub refuses that pass everything.
    fs::remove_file(b.path("s1-ending")).unwrap();
    b.pass().run("sam/box").unwrap();
    let job = Job::new(&b.cfg, "s1");
    let kept = settled(&d, &job).expect("settled by the pass");
    assert!(kept.answer.unwrap().starts_with("later"));
    assert_eq!(slots(&b), (vec![], vec![]));
    // A later turn's answer isn't the kept one's.
    job.set_turns(2).unwrap();
    assert!(settled(&d, &job).is_none());
}
