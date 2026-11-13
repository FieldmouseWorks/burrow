//! Worker and sorting jobs stop when their issue may no longer be read.
use super::{Box, issue, on_github, sorting};
use serde_json::json;
use std::fs;

// A job, a sorting job's included, whose issue an agent may no longer read
// is stopped even while its turn waits for a slot, before it reads any.
#[test]
fn a_queued_job_is_stopped_once_its_issue_is_barred() {
    for barred in [
        issue(7, "sam", &["claude", "outside text"]),
        issue(7, "former", &["claude"]),
    ] {
        let b = Box::new();
        b.job("j1", "done", "");
        fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
        fs::write(b.path("active"), "").unwrap();
        let d = b.record("j1");
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&barred))
            .unwrap();
        assert!(!d.exists(), "{barred}");
        assert!(b.cfg.jobs.join("j1/1.stopped").exists(), "{barred}");

        let b = Box::new();
        let d = sorting(&b, "");
        fs::remove_file(b.cfg.jobs.join("s1/1.outcome")).unwrap();
        fs::write(b.path("active"), "").unwrap();
        let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
        let mut unlabelled = barred.clone();
        unlabelled["labels"] = json!(
            barred["labels"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|l| l["name"] != "claude")
                .collect::<Vec<_>>()
        );
        b.pass()
            .verdicts("sam/box", &t, std::slice::from_ref(&unlabelled))
            .unwrap();
        assert!(!d.exists(), "{unlabelled}");
        assert!(b.cfg.jobs.join("s1/1.stopped").exists(), "{unlabelled}");
    }
}

// Barred while its label comes off or it closes, so the pass's list no
// longer has it: GitHub is asked, and a job still to finish is stopped. One
// closed that may still be read finishes, as before.
#[test]
fn a_barred_issue_off_the_list_stops_its_job() {
    let seven = |labels: &[&str]| issue(7, "sam", labels);
    for (now, worker_stopped, stopped) in [
        (seven(&["outside text"]), true, true),
        (issue(7, "former", &[]), true, true),
        (seven(&[]), true, false),
        (seven(&["claude"]), false, false),
    ] {
        let mut now = now;
        now["repository_url"] = json!("https://api.github.com/repos/sam/box");
        let b = Box::new();
        b.job("j1", "done", "");
        fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
        fs::write(b.path("active"), "").unwrap();
        on_github(&b, &now);
        let d = b.record("j1");
        b.pass().jobs("sam/box", "box", &[]).unwrap();
        assert_eq!(!d.exists(), worker_stopped, "{now}");
        assert_eq!(
            b.cfg.jobs.join("j1/1.stopped").exists(),
            worker_stopped,
            "{now}"
        );

        let b = Box::new();
        let d = sorting(&b, "");
        fs::remove_file(b.cfg.jobs.join("s1/1.outcome")).unwrap();
        fs::write(b.path("active"), "").unwrap();
        on_github(&b, &now);
        let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
        b.pass().verdicts("sam/box", &t, &[]).unwrap();
        assert_eq!(!d.exists(), stopped, "{now}");
    }
    // GitHub doesn't say: kept, and asked again.
    let b = Box::new();
    b.job("j1", "done", "");
    fs::remove_file(b.cfg.jobs.join("j1/1.outcome")).unwrap();
    fs::write(b.path("active"), "").unwrap();
    let d = b.record("j1");
    fs::write(b.path("fail"), "repos/sam/box/issues/7\n").unwrap();
    let mut p = b.pass();
    p.jobs("sam/box", "box", &[]).unwrap();
    assert!(d.exists() && p.wake.contains_key("sam/box"));
}
