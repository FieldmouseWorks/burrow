//! Lock file refreshes lock-age refused for age only: asked for again, a
//! request taken back, and a job once that's done with.

use super::*;

#[test]
fn a_refresh_that_waits_for_its_releases_is_asked_for_again_three_times() {
    // A lock file refresh lock-age refused only because nothing in it is
    // old enough yet goes to no job: it waits, then Renovate is asked to
    // refresh it again (Renovate's rebase label), and asked again only when a
    // request went without anything moving, three times at most.
    let m = Machine::new();
    let (later, past) = (now() + 3 * 86400, now() - 60);
    let refresh = |ripe| m.prs(&[m.refresh(20, ripe, "me", LOCK)]);
    refresh(later);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    assert!(p.log.contains(&"lock-wait me".to_string()), "{:#?}", p.log);
    refresh(past);
    let p = m.pass();
    assert!(p.jobs.is_empty());
    assert_eq!(p.asked(), rebase(20));
    let key = m.key(20, "");
    let asked = key
        .strip_prefix(&format!("{} refreshed {past} 1 ", sha(20)))
        .unwrap();
    assert!(
        now() - asked.trim_end().parse::<u64>().unwrap() < 60,
        "{key}"
    );
    // Renovate's run comes within the hour: nothing more till then.
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    // Taken and dropped with nothing to show for it: asked again, three
    // times in all, then it's a job's ...
    for _ in 0..2 {
        m.asked_ago(20, 7200);
        let p = m.pass();
        assert!(p.jobs.is_empty());
        assert_eq!(p.asked(), rebase(20));
    }
    m.asked_ago(20, 7200);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    // ... once a Renovate run that took the last one may be over.
    m.released(20, 7100);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    m.released(20, 7300);
    let p = m.pass();
    assert!(p.task().contains("#20 \"Lock file maintenance (cargo)\" (branch renovate/cargo-lock-file-maintenance): its CI fails\n"));
    assert!(p.asked().is_empty());
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty(), "one job a head");
}

#[test]
fn a_request_standing_six_hours_is_taken_back_for_a_job() {
    // Once it's taken back and a Renovate run that read it is surely over;
    // a new time on the same head starts the requests over.
    let m = Machine::new();
    let past = now() - 60;
    m.prs(&[m.refresh(22, past, "me", LOCK)]);
    m.pass();
    m.asked_ago(22, 7200);
    m.prs(&[requested(m.refresh(22, past, "me", LOCK))]);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    m.asked_ago(22, 7 * 3600);
    m.prs(&[m.refresh(22, past - 3600, "me", LOCK)]);
    assert_eq!(m.pass().asked(), rebase(22));
    m.asked_ago(22, 7 * 3600);
    m.prs(&[requested(m.refresh(22, past - 3600, "me", LOCK))]);
    let p = m.pass();
    assert!(p.jobs.is_empty());
    assert_eq!(p.asked(), unasked(22));
    // Taken back: no more asking on that head, and no job till the run is
    // over.
    m.prs(&[m.refresh(22, past - 3600, "me", LOCK)]);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    m.released(22, 7300);
    let p = m.pass();
    assert!(p.task().contains("#22 ") && !p.task().contains("rebase"));
    assert!(p.asked().is_empty());
}

#[test]
fn a_request_github_refused_counts_as_asked_all_the_same() {
    // Three of them, then it's a job's, not a request every pass for good.
    let m = Machine::new();
    let past = now() - 60;
    m.set("gh-refuses", "");
    m.prs(&[m.refresh(24, past, "me", LOCK)]);
    for _ in 0..3 {
        assert_eq!(m.pass().asked(), rebase(24));
        let p = m.pass();
        assert!(p.jobs.is_empty() && p.asked().is_empty());
        m.asked_ago(24, 7200);
    }
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    m.released(24, 7300);
    let p = m.pass();
    assert!(p.task().contains("#24 ") && p.asked().is_empty());
}

#[test]
fn a_request_github_wont_take_back_goes_to_the_job_to_take_back() {
    // One that went through though GitHub didn't say so stands, as any
    // request; one GitHub won't take back is tried three times, ten
    // minutes apart, then the job gets the PR with the request to take
    // back itself.
    let m = Machine::new();
    let past = now() - 60;
    m.set("gh-refuses", "");
    m.prs(&[m.refresh(25, past, "me", LOCK)]);
    m.pass();
    m.asked_ago(25, 7200);
    m.prs(&[requested(m.refresh(25, past, "me", LOCK))]);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    m.asked_ago(25, 7 * 3600);
    for _ in 0..3 {
        assert_eq!(m.pass().asked(), unasked(25));
        let p = m.pass();
        assert!(p.jobs.is_empty() && p.asked().is_empty());
        m.released(25, 700);
    }
    let p = m.pass();
    assert!(p.task().contains("#25 \"Lock file maintenance (cargo)\" (branch renovate/cargo-lock-file-maintenance): its CI fails; it has the rebase label, which has Renovate redo the branch over what you push: take the label off first, and push nothing to it for 120 minutes after, while a Renovate run that read it may still be going\n"), "{}", p.task());
    assert!(p.asked().is_empty());
}

#[test]
fn whatever_else_sends_a_pr_with_a_request_to_a_job_takes_the_request_off_first() {
    // Another red check, a failure that isn't only age, a major: the
    // request comes off first, and the job waits out the run.
    let later = now() + 3 * 86400;
    let title = "Update foo26 to v2";
    for why in ["check", "failure", "major"] {
        let m = Machine::new();
        let mut p = requested(m.refresh(26, later, "me", title));
        match why {
            "check" => p["statusCheckRollup"][0]["conclusion"] = "FAILURE".into(),
            "failure" => m.unset("rest-26"),
            _ => {
                p["statusCheckRollup"].as_array_mut().unwrap().truncate(1);
                p["labels"] = json!([{"name": "major"}, {"name": "rebase"}]);
            }
        }
        m.prs(&[p.clone()]);
        let got = m.pass();
        assert!(got.jobs.is_empty(), "{why}");
        assert_eq!(got.asked(), unasked(26), "{why}");
        p["labels"]
            .as_array_mut()
            .unwrap()
            .retain(|l| l["name"] != "rebase");
        m.prs(&[p]);
        let got = m.pass();
        assert!(got.jobs.is_empty() && got.asked().is_empty(), "{why}");
        m.released(26, 7300);
        let got = m.pass();
        assert!(
            got.task().contains("#26 ") && got.asked().is_empty(),
            "{why}"
        );
    }
}

#[test]
fn only_renovates_own_word_on_a_refresh_makes_it_wait() {
    let later = now() + 3 * 86400;
    // As the machine's own App: only its bot's comment counts, not a user
    // who holds its name.
    let m = Machine::as_app();
    m.prs(&[m.refresh(23, later, "sam-workers[bot]", LOCK)]);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty());
    assert!(
        p.log.contains(&"lock-wait sam-workers[bot]".to_string()),
        "{:#?}",
        p.log
    );
    let m = Machine::as_app();
    m.prs(&[m.refresh(23, later, "sam-workers", LOCK)]);
    assert!(m.pass().task().contains("#23 "));
    // Renovate as an App of its own: its bot's word counts, and the jobs'
    // App's, which is no longer Renovate, doesn't.
    let m = Machine::renovate_apart();
    m.prs(&[m.refresh(23, later, "sam-renovate[bot]", LOCK)]);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.asked().is_empty(), "{:#?}", p.jobs);
    assert!(
        p.log.contains(&"lock-wait sam-renovate[bot]".to_string()),
        "{:#?}",
        p.log
    );
    let m = Machine::renovate_apart();
    m.prs(&[m.refresh(23, later, "sam-workers[bot]", LOCK)]);
    assert!(m.pass().task().contains("#23 "));
    // Anything lock-wait doesn't take (someone else's comment, no comment,
    // a failure that isn't age), or another red check: red CI, a job's to
    // fix.
    for by in ["mallory", ""] {
        let m = Machine::new();
        let p = m.refresh(21, later, "mallory", LOCK);
        if by.is_empty() {
            m.unset("rest-21");
        }
        m.prs(&[p]);
        let got = m.pass();
        assert!(
            got.task().contains("#21 ") && got.asked().is_empty(),
            "{by:?}"
        );
    }
    let m = Machine::new();
    let mut p = m.refresh(21, later, "me", LOCK);
    p["statusCheckRollup"][0]["conclusion"] = "FAILURE".into();
    m.prs(&[p]);
    let got = m.pass();
    assert!(got.task().contains("#21 ") && got.asked().is_empty());
    assert!(
        !got.log.iter().any(|l| l.starts_with("lock-wait")),
        "{:#?}",
        got.log
    );
}

/// PR `n`'s timeline: a request that came off `ago` seconds back, by
/// `event` (unlabeled, or renamed from a rebase! title).
fn gone(m: &Machine, n: u64, event: &str, ago: i64) {
    let at = jiff::Timestamp::from_second(now() as i64 - ago).unwrap();
    let e = match event {
        "unlabeled" => {
            json!({"event": "unlabeled", "created_at": at.to_string(), "label": {"name": "rebase"}})
        }
        _ => json!({"event": "renamed", "created_at": at.to_string(),
                    "rename": {"from": "rebase!Update foo27 to v2", "to": "Update foo27 to v2"}}),
    };
    // Other events, and another label coming off, count for nothing.
    let other = json!({"event": "unlabeled", "created_at": jiff::Timestamp::now().to_string(), "label": {"name": "major"}});
    m.set(&format!("timeline-{n}"), &json!([[e], [other]]).to_string());
}

#[test]
fn a_rebase_title_comes_off_on_sight_and_its_run_is_waited_out() {
    // Renovate redoes the branch on every run while a title starts
    // rebase!, ours or anyone's, and each push starts the next: the sweep
    // takes it off and hands the PR to no job till a run that read it is
    // over.
    let m = Machine::new();
    let mut p = pr_with(27, &[], "FAILURE");
    p["title"] = "rebase!Update foo27 to v2".into();
    m.prs(&[p]);
    let got = m.pass();
    assert!(got.jobs.is_empty(), "{:#?}", got.jobs);
    assert_eq!(
        got.asked(),
        [format!(
            "PATCH repos/{REPO}/pulls/27 -f title=Update foo27 to v2"
        )]
    );
    m.prs(&[pr_with(27, &[], "FAILURE")]);
    gone(&m, 27, "renamed", 600);
    let got = m.pass();
    assert!(got.jobs.is_empty() && got.asked().is_empty());
    gone(&m, 27, "renamed", 7300);
    assert!(m.pass().task().contains("#27 "));
}

#[test]
fn a_request_that_came_off_between_passes_is_waited_out_on_any_head() {
    // Renovate took the label (a request no pass saw standing, someone
    // else's, or on a head since replaced): a run that read it may be
    // going still, so no job for a run's length after.
    let m = Machine::new();
    m.prs(&[pr_with(28, &[], "FAILURE")]);
    gone(&m, 28, "unlabeled", 600);
    let got = m.pass();
    assert!(got.jobs.is_empty() && got.asked().is_empty());
    gone(&m, 28, "unlabeled", 7300);
    assert!(m.pass().task().contains("#28 "));
}

#[test]
fn a_timeline_that_cant_be_read_holds_the_pr() {
    // Whether a request just came off it can't say: no job this pass.
    let m = Machine::new();
    m.prs(&[pr_with(29, &[], "FAILURE")]);
    m.set("timeline-29", "not json");
    assert!(m.pass().jobs.is_empty());
    m.unset("timeline-29");
    // Nor when gh failed, whatever it printed: --slurp prints what pages it
    // got, none or some, as a list all the same.
    m.set("timeline-fails", "");
    assert!(m.pass().jobs.is_empty());
    m.unset("timeline-fails");
    assert!(m.pass().task().contains("#29 "));
}

// No GitHub event is needed at the age boundary: the sweep books a pass.
#[test]
fn a_wait_books_its_date_and_keeps_other_repos_wakes() {
    let m = Machine::new();
    let ripe = now() + 120;
    m.prs(&[m.refresh(20, ripe, "me", LOCK)]);
    m.pass();
    let wake = m.path("state/wake.json");
    let booked: Value = serde_json::from_slice(&fs::read(&wake).unwrap()).unwrap();
    assert_eq!(booked[REPO], ripe);
    let mut booked = booked;
    booked["sam/other"] = json!(ripe + 30);
    fs::write(&wake, booked.to_string()).unwrap();
    m.prs(&[m.refresh(20, now() - 60, "me", LOCK)]);
    assert_eq!(m.pass().asked(), rebase(20));
    let booked: Value = serde_json::from_slice(&fs::read(&wake).unwrap()).unwrap();
    assert_eq!(booked["sam/other"], ripe + 30);
    let retry = booked[REPO].as_u64().unwrap();
    assert!(retry >= now() + 3590 && retry <= now() + 3600);
    m.prs(&[]);
    m.pass();
    let booked: Value = serde_json::from_slice(&fs::read(&wake).unwrap()).unwrap();
    assert!(booked.get(REPO).is_none());
    assert_eq!(booked["sam/other"], ripe + 30);
}

#[test]
fn the_retry_wake_uses_the_watchdogs_exported_limit() {
    let m = Machine::new();
    m.set("limits", "3 21600 3 600 45\n");
    m.prs(&[m.refresh(20, now() - 60, "me", LOCK)]);
    assert_eq!(m.pass().asked(), rebase(20));
    let booked: Value =
        serde_json::from_slice(&fs::read(m.path("state/wake.json")).unwrap()).unwrap();
    let retry = booked[REPO].as_u64().unwrap();
    assert!(retry >= now() + 40 && retry <= now() + 45);
}
