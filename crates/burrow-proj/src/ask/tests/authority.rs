//! The registry is the authority on a shared question's policy: an error in
//! it is never read as no question, every decision on policy reads it under
//! its lock, an accepted answer retires a member at once, a pause gives back
//! its time through it, and every notice renders it as it is now.
use super::*;

fn registry(cfg: &Config) -> std::path::PathBuf {
    control_root(cfg).join(".about/state.json")
}

pub(super) fn edit_group(cfg: &Config, key: &str, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(registry(cfg)).unwrap()).unwrap();
    edit(&mut state[key]);
    fs::write(registry(cfg), state.to_string()).unwrap();
}

pub(super) fn group_due(cfg: &Config, key: &str) -> i64 {
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(registry(cfg)).unwrap()).unwrap();
    state[key]["due"].as_i64().unwrap()
}

/// An accepted answer to `id`'s question whose cleanup never ran.
fn accepted_unapplied(cfg: &Config, id: &str, named: bool, deadline: bool) {
    let job = Job::new(cfg, id);
    let question = named.then(|| about::question_id(&job));
    fs::write(
        host_path(&job, "resume-2"),
        serde_json::json!({
            "through": 1, "answered": true, "clear": true, "deadline": deadline,
            "accepted": true, "applied": false, "question": question,
        })
        .to_string(),
    )
    .unwrap();
}

/// The body of the last `ensure` the notifier was given for `key`.
fn last_ensure(dir: &tempfile::TempDir, key: &str) -> String {
    let calls = calls(dir);
    let at = calls
        .rfind(&format!("ensure|{key}|"))
        .unwrap_or_else(|| panic!("no ensure for {key}: {calls}"));
    let body = &calls[at + format!("ensure|{key}|").len()..];
    let end = ["\nensure|", "\nclose|", "\npending|"]
        .iter()
        .filter_map(|s| body.find(s))
        .min()
        .unwrap_or(body.len());
    body[..end].to_owned()
}

#[test]
fn an_unreadable_registry_holds_every_question_and_refuses_what_needs_its_policy() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "keyed", Some("profile"), false);
    asks(&cfg, "other", Some("profile"), false);
    asks(&cfg, "plain", None, false);
    let idle = Job::new(&cfg, "idle");
    fs::create_dir_all(&idle.dir).unwrap();
    fs::write(registry(&cfg), "{").unwrap();
    for id in ["keyed", "plain"] {
        let job = Job::new(&cfg, id);
        assert!(load(&job).is_err(), "{id}: an error read as no question");
        assert!(open(&job).is_err(), "{id}");
        // Its scratch and worktrees stay: it still waits.
        assert!(!crate::scratch::over(&job), "{id}");
        let _held = job.lock().unwrap();
        // The deadline can't be decided: a retry, never a default or release.
        assert!(expire(&cfg, &job).is_err(), "{id}");
        assert_eq!(job.turns().as_deref(), Some("1"), "{id}");
        assert!(!host_path(&job, "question-needs-owner").exists(), "{id}");
        // Its notice is neither closed nor sent without its policy.
        assert!(crate::question::notify(&cfg, &job).is_err(), "{id}");
        assert!(!job.turn("1", "question-close").exists(), "{id}");
    }
    let keyed = Job::new(&cfg, "keyed");
    // Its label stays as it was: the error is the retry.
    assert!(crate::ask::visibility(&cfg, &keyed).is_err());
    // A reply that can't name who else waits on it resumes nobody.
    assert!(cohort(&keyed).is_err());
    assert!(crate::cmd::reply_answer(&cfg, "keyed", "yes", false).is_err());
    for id in ["keyed", "other"] {
        assert_eq!(Job::new(&cfg, id).turns().as_deref(), Some("1"), "{id}");
    }
    // Machine news can't resume it either: whether it still asks is unknown.
    assert!(crate::question::Resume::prepare(&keyed, false).is_err());
    // The owner still hears of each, never fewer.
    assert_eq!(owner_lines(&cfg).len(), 3);
    // A question that can't be read can't name its cohort either.
    fs::write(registry(&cfg), "{}").unwrap();
    fs::write(host_path(&keyed, "question"), "{").unwrap();
    assert!(cohort(&keyed).is_err());
    assert!(follows(&keyed).is_err());
    // A job with no question needs no registry.
    assert!(load(&idle).unwrap().is_none());
    assert!(follows(&idle).unwrap().is_none());
    assert!(
        crate::cmd::told(&cfg, "keyed", "1")
            .unwrap()
            .contains("can't be read")
    );
}

#[test]
fn a_move_reads_the_policy_its_group_has_under_the_lock_not_the_one_read_before() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "a", Some("source"), false);
    let a = Job::new(&cfg, "a");
    // a's finish read its policy here, before the registry's lock...
    let stale = open(&a).unwrap().unwrap();
    assert!(!stale.recommendation.one_way);
    // ...then b joined its subject one-way, and later its deadline moved.
    asks(&cfg, "b", Some("source"), true);
    let due = group_due(&cfg, "source") + 900;
    edit_group(&cfg, "source", |g| g["due"] = due.into());
    about::publish(&a, "fresh", stale, |q| {
        fs::write(host_path(&a, "question"), serde_json::to_string(q).unwrap())
            .map_err(|e| e.to_string())
    })
    .unwrap();
    let moved = load(&a).unwrap().unwrap();
    assert_eq!(moved.recommendation.about.as_deref(), Some("fresh"));
    assert!(moved.recommendation.one_way, "a stale read downgraded it");
    assert_eq!(moved.due, due);
}

#[test]
fn a_default_waits_on_the_registry_and_takes_the_policy_it_has_then() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "j1", Some("profile"), false);
    let past = crate::now() - 1;
    edit_group(&cfg, "profile", |g| g["due"] = past.into());
    // A join holds the registry while it makes the group one-way.
    let join = about::locked(&cfg).unwrap();
    let decided = std::thread::spawn({
        let cfg = cfg.clone();
        move || {
            let job = Job::new(&cfg, "j1");
            let _held = job.lock().unwrap();
            expire(&cfg, &job).unwrap()
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    edit_group(&cfg, "profile", |g| g["one_way"] = true.into());
    drop(join);
    assert_eq!(decided.join().unwrap(), None);
    let j1 = Job::new(&cfg, "j1");
    assert!(
        host_path(&j1, "question-needs-owner").exists(),
        "a reversible default was taken for a one-way group"
    );
    assert!(!j1.path("prompt.2").exists());
}

#[test]
fn letting_go_of_the_registry_frees_it_while_a_forked_child_shares_it() {
    use std::os::fd::AsFd;
    let (_dir, cfg) = fixture();
    let held = about::locked(&cfg).unwrap();
    // A dup shares the open file description just as a descriptor in a
    // concurrently forked child does, until that child reaches exec.
    let inherited = held.as_fd().try_clone_to_owned().unwrap();
    drop(held);
    let probe = fs::File::options()
        .append(true)
        .open(registry(&cfg).with_file_name("lock"))
        .unwrap();
    probe
        .try_lock()
        .expect("the registry stayed locked after its holder let go");
    probe.unlock().unwrap();
    drop(inherited);
}

#[test]
fn default_admission_releases_the_registry_for_concurrent_publication() {
    use std::fs::{OpenOptions, TryLockError};
    use std::time::{Duration, Instant};

    for (expiring, scanned) in [("a", "z"), ("z", "a")] {
        for upgrade in [false, true] {
            for slots in [1, 4] {
                let (dir, mut cfg) = fixture();
                let inbox = cfg.inbox.as_mut().unwrap();
                inbox.state = dir.path().join("inbox");
                inbox.wake = dir.path().join("inbox-wake");
                inbox.triage = Some(crate::config::Triage {
                    sort_unlabelled: true,
                    owner_label: "owner".into(),
                    owner: None,
                    skip: vec![],
                    at_once: slots,
                    drain: None,
                    drain_jobs: 4,
                });
                for id in [expiring, scanned] {
                    asks(&cfg, id, Some("subject"), false);
                    let record = cfg.inbox.as_ref().unwrap().state.join(".triage").join(id);
                    fs::create_dir_all(&record).unwrap();
                    fs::write(record.join("job"), id).unwrap();
                }
                edit_group(&cfg, "subject", |g| g["due"] = (crate::now() - 1).into());
                let other = Job::new(&cfg, scanned);
                let held = other.lock().unwrap();
                let open_lock = |path| {
                    OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path)
                        .unwrap()
                };
                let authority = open_lock(registry(&cfg).with_file_name("lock"));
                let admission = open_lock(dir.path().join("inbox-wake.lock"));
                let running = std::thread::spawn({
                    let cfg = cfg.clone();
                    move || {
                        let job = Job::new(&cfg, expiring);
                        let _held = job.lock().unwrap();
                        expire(&cfg, &job)
                    }
                });
                // A blocked sorting scan must leave the registry available to
                // the publisher holding the job it is inspecting. Observe the
                // locks themselves, and release z even on the broken ordering.
                // The scan blocks on z until we drop it, so wait for that
                // state, not a time: a thread that finished never blocked on
                // z. The bound only turns a scan stuck elsewhere into a
                // failure instead of a hang, and is far past any load.
                let until = Instant::now() + Duration::from_secs(300);
                let scans_first = loop {
                    match authority.try_lock() {
                        Ok(()) => authority.unlock().unwrap(),
                        Err(TryLockError::WouldBlock) => break false,
                        Err(e) => panic!("{e}"),
                    }
                    match admission.try_lock() {
                        Ok(()) => admission.unlock().unwrap(),
                        Err(TryLockError::WouldBlock) => {
                            // Both observations must describe the blocked scan:
                            // it may have taken both locks between our probes.
                            break match authority.try_lock() {
                                Ok(()) => {
                                    authority.unlock().unwrap();
                                    true
                                }
                                Err(TryLockError::WouldBlock) => false,
                                Err(e) => panic!("{e}"),
                            };
                        }
                        Err(e) => panic!("{e}"),
                    }
                    if running.is_finished() || Instant::now() >= until {
                        break false;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                };
                if scans_first && upgrade {
                    // This publication can finish while z is locked, and its
                    // upgrade must bind the default after admission completes.
                    asks(&cfg, "upgrade", Some("subject"), true);
                }
                drop(held);
                running.join().unwrap().unwrap();
                assert!(
                    scans_first,
                    "default held the registry while scanning job locks"
                );
                let a = Job::new(&cfg, expiring);
                assert_eq!(a.turns().as_deref(), Some("2"));
                if upgrade {
                    assert!(host_path(&a, "question-needs-owner").exists());
                    assert!(!a.path("prompt.2").exists());
                } else {
                    assert!(
                        read(&a.path("prompt.2"))
                            .unwrap()
                            .contains("Take your reversible")
                    );
                    assert!(load(&a).unwrap().is_none());
                    assert_eq!(crate::policy::parked(&a), slots == 1);
                }
            }
        }
    }
}

#[test]
fn an_accepted_answer_retires_its_member_before_its_cleanup_runs() {
    for (named, deadline, merging) in [true, false].into_iter().flat_map(|named| {
        [true, false]
            .into_iter()
            .flat_map(move |deadline| [true, false].map(move |merging| (named, deadline, merging)))
    }) {
        let (_dir, cfg) = fixture();
        asks(&cfg, "old", Some("profile"), !deadline);
        let past = crate::now() - 1;
        edit_group(&cfg, "profile", |g| g["due"] = past.into());
        let old = Job::new(&cfg, "old");
        // Answered and accepted; the process died before its cleanup.
        accepted_unapplied(&cfg, "old", named, deadline);
        assert!(host_path(&old, "question").exists());
        assert!(load(&old).unwrap().is_none(), "named {named}");
        assert!(open(&old).unwrap().is_none(), "named {named}");
        // A new question on the subject starts a fresh group: its own
        // deadline and requirement, not the retired one's, and it leads.
        asks(&cfg, "new", (!merging).then_some("profile"), false);
        if merging {
            merge(&cfg, "profile", &["new".into()]).unwrap();
        }
        let new = Job::new(&cfg, "new");
        let q = load(&new).unwrap().unwrap();
        assert!(!q.recommendation.one_way, "named {named}");
        assert!(q.due > crate::now(), "named {named}");
        assert!(follows(&new).unwrap().is_none());
        assert!(cohort(&new).unwrap().is_empty());
        {
            let _held = new.lock().unwrap();
            assert_eq!(expire(&cfg, &new).unwrap(), Some(q.due));
        }
        // Merging into it reads the same rule.
        asks(&cfg, "third", None, false);
        merge(&cfg, "profile", &["third".into()]).unwrap();
        let waiting: Vec<String> = cohort(&Job::new(&cfg, "third"))
            .unwrap()
            .into_iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(waiting, ["new"]);
    }
    // An intent not yet accepted retires nothing.
    let (_dir, cfg) = fixture();
    asks(&cfg, "old", Some("profile"), false);
    let old = Job::new(&cfg, "old");
    crate::question::Resume::prepare(&old, true).unwrap();
    asks(&cfg, "new", Some("profile"), false);
    assert_eq!(
        follows(&Job::new(&cfg, "new")).unwrap().as_deref(),
        Some("old")
    );
}

#[test]
fn independently_paused_members_keep_their_remaining_time_after_failed_restoration() {
    let (_dir, cfg) = fixture();
    for id in ["a", "b"] {
        asks(&cfg, id, Some("subject"), false);
    }
    let due = group_due(&cfg, "subject");
    let now = crate::now();
    let mut a = park(&cfg, "a", now - 600, Some(due));
    let mut b = park(&cfg, "b", now - 900, Some(due));
    let a_job = Job::new(&cfg, "a");
    let b_job = Job::new(&cfg, "b");
    crate::policy::shift_waits(&a_job, &mut a).unwrap();
    let blocked = registry(&cfg).with_file_name("state.json.new");
    fs::create_dir(&blocked).unwrap();
    let before = serde_json::to_string(&b).unwrap();
    assert!(crate::policy::shift_waits(&b_job, &mut b).is_err());
    assert_eq!(serde_json::to_string(&b).unwrap(), before);
    fs::remove_dir(blocked).unwrap();
    for _ in 0..2 {
        crate::policy::shift_waits(&b_job, &mut b).unwrap();
        let effective = load(&b_job).unwrap().unwrap().due;
        assert!((due + 900..=due + 905).contains(&effective));
    }
    // A later, separate pause extends from the deadline then in force.
    let effective = load(&a_job).unwrap().unwrap().due;
    // Simulate a genuinely later pause: it must not overlap the pause that
    // just ended. Moving the test clock forward keeps the exact 300s check.
    restore(&a_job, now + 100, now + 400).unwrap();
    crate::cmd::stop(&cfg, "a", false).unwrap();
    merge(&cfg, "moved", &["b".into()]).unwrap();
    let moved = load(&b_job).unwrap().unwrap().due;
    assert!((effective + 300..=effective + 305).contains(&moved));
}

pub(super) fn park(cfg: &Config, id: &str, at: i64, due: Option<i64>) -> crate::policy::Park {
    // Exercise migration of both historical park formats, with and without
    // the old deadline snapshot. New park records need neither baseline.
    let legacy = serde_json::json!({"state":"done", "turn":"1", "first":false, "at":at, "due":due});
    let rec: crate::policy::Park = serde_json::from_value(legacy.clone()).unwrap();
    fs::create_dir_all(
        crate::policy::path(&Job::new(cfg, id), "park")
            .parent()
            .unwrap(),
    )
    .unwrap();
    fs::write(
        crate::policy::path(&Job::new(cfg, id), "park"),
        legacy.to_string(),
    )
    .unwrap();
    rec
}

#[test]
fn a_restored_job_gets_back_its_paused_time_once_through_the_shared_deadline() {
    let (_dir, cfg) = fixture();
    asks(&cfg, "j1", Some("profile"), false);
    asks(&cfg, "j2", Some("profile"), false);
    asks(&cfg, "solo", Some("alone"), false);
    asks(&cfg, "plain", None, false);
    let due = group_due(&cfg, "profile");
    let paused = crate::now() - 600;
    let near = |got: i64, want: i64| (want..=want + 5).contains(&got);
    // A sole member: its deadline moves out by the time it was parked.
    let solo_due = group_due(&cfg, "alone");
    let mut rec = park(&cfg, "solo", paused, Some(solo_due));
    let solo = Job::new(&cfg, "solo");
    crate::policy::shift_waits(&solo, &mut rec).unwrap();
    assert!(near(load(&solo).unwrap().unwrap().due, solo_due + 600));
    // Two members parked together extend their shared deadline once, and a
    // failed restoration retried with its record extends it no further.
    for id in ["j1", "j2", "j1"] {
        let mut rec = park(&cfg, id, paused, Some(due));
        crate::policy::shift_waits(&Job::new(&cfg, id), &mut rec).unwrap();
        assert!(
            near(load(&Job::new(&cfg, id)).unwrap().unwrap().due, due + 600),
            "{id}"
        );
    }
    for id in ["j1", "j2"] {
        assert!(
            near(load(&Job::new(&cfg, id)).unwrap().unwrap().due, due + 600),
            "{id}"
        );
    }
    // The leader stopping hands over the same deadline; a merge to a fresh
    // subject carries it.
    crate::cmd::stop(&cfg, "j1", false).unwrap();
    merge(&cfg, "elsewhere", &["j2".into()]).unwrap();
    assert!(near(
        load(&Job::new(&cfg, "j2")).unwrap().unwrap().due,
        due + 600
    ));
    // A legacy park has no deadline snapshot. Its authority records the
    // interval once, even when the unchanged legacy record is retried.
    let plain = Job::new(&cfg, "plain");
    let plain_due = load(&plain).unwrap().unwrap().due;
    let legacy = park(&cfg, "plain", paused, None);
    for _ in 0..2 {
        let mut rec: crate::policy::Park =
            serde_json::from_slice(&fs::read(crate::policy::path(&plain, "park")).unwrap())
                .unwrap();
        assert_eq!(rec.at, legacy.at);
        crate::policy::shift_waits(&plain, &mut rec).unwrap();
        // An unkeyed question's own deadline moves as it always has.
        assert!(near(load(&plain).unwrap().unwrap().due, plain_due + 600));
    }
}

#[test]
fn every_send_of_a_notice_says_the_policy_it_has_now_once() {
    let (dir, cfg) = fixture();
    asks(&cfg, "j1", None, false);
    notified(&cfg, &["j1"]);
    let first = load(&Job::new(&cfg, "j1")).unwrap().unwrap();
    let body = last_ensure(&dir, "proj-question-j1-1");
    assert!(
        body.contains(&burrow_probe::answer::utc(first.due)),
        "{body}"
    );
    // Merged into an occupied subject due sooner, under a one-way member.
    asks(&cfg, "j2", Some("profile"), true);
    let soon = crate::now() + 3600;
    edit_group(&cfg, "profile", |g| g["due"] = soon.into());
    merge(&cfg, "profile", &["j1".into()]).unwrap();
    notified(&cfg, &["j1"]);
    let body = last_ensure(&dir, "proj-question-j1-1");
    assert!(body.contains(&burrow_probe::answer::utc(soon)), "{body}");
    assert!(
        !body.contains(&burrow_probe::answer::utc(first.due)),
        "{body}"
    );
    assert!(body.contains("This is one-way"), "{body}");
    assert_eq!(body.matches("Without an answer by").count(), 1, "{body}");
    assert!(
        body.contains("May j1 edit the profile?"),
        "the question text was lost: {body}"
    );
    assert!(body.contains("1 other job: j2"), "{body}");
    // A notice cached with its policy (as before) keeps its text and says
    // only the current policy.
    let cached = Job::new(&cfg, "j1").turn("1", "question");
    let legacy = format!(
        "Job j1 needs your answer. Reply with proj reply j1.\n\nMay j1 edit the profile?{}",
        first.notice()
    );
    fs::write(&cached, legacy).unwrap();
    notified(&cfg, &["j1"]);
    let body = last_ensure(&dir, "proj-question-j1-1");
    assert_eq!(body.matches("Without an answer by").count(), 1, "{body}");
    assert!(body.contains(&burrow_probe::answer::utc(soon)), "{body}");
    assert!(body.contains("May j1 edit the profile?"), "{body}");
    // A released one-way question's notice says its policy too.
    let (dir, cfg) = fixture();
    asks(&cfg, "r", None, true);
    let r = Job::new(&cfg, "r");
    let mut q = load_raw(&r).unwrap();
    q.due = crate::now() - 1;
    fs::write(
        host_path(&r, "question"),
        serde_json::to_string(&q).unwrap(),
    )
    .unwrap();
    {
        let _held = r.lock().unwrap();
        expire(&cfg, &r).unwrap();
    }
    notified(&cfg, &["r"]);
    let body = last_ensure(&dir, "proj-question-r-1");
    assert_eq!(body.matches("This is one-way").count(), 1, "{body}");
    assert!(body.contains(&burrow_probe::answer::utc(q.due)), "{body}");
}

#[test]
fn notices_refresh_after_joins_pause_restoration_handover_and_moves() {
    let (dir, cfg) = fixture();
    asks(&cfg, "a", Some("subject"), false);
    notified(&cfg, &["a"]);
    asks(&cfg, "b", Some("subject"), true);
    let a = Job::new(&cfg, "a");
    let before = load(&a).unwrap().unwrap().due;
    let mut rec = park(&cfg, "a", crate::now() - 600, Some(before));
    crate::policy::shift_waits(&a, &mut rec).unwrap();
    let shifted = load(&a).unwrap().unwrap().due;
    let check = |id: &str, due: i64| {
        notified(&cfg, &[id]);
        let body = last_ensure(&dir, &format!("proj-question-{id}-1"));
        assert!(
            body.contains(&format!("May {id} edit the profile?")),
            "{body}"
        );
        assert!(body.contains(&burrow_probe::answer::utc(due)), "{body}");
        assert_eq!(body.matches("Without an answer by").count(), 1, "{body}");
        assert_eq!(body.matches("This is one-way").count(), 1, "{body}");
    };
    check("a", shifted);
    crate::cmd::stop(&cfg, "a", false).unwrap();
    check("b", shifted);
    merge(&cfg, "fresh", &["b".into()]).unwrap();
    check("b", shifted);
    // A legacy released publication has no saved question-released record.
    let b = Job::new(&cfg, "b");
    edit_group(&cfg, "fresh", |g| g["due"] = (crate::now() - 1).into());
    let due = load(&b).unwrap().unwrap().due;
    {
        let _held = b.lock().unwrap();
        expire(&cfg, &b).unwrap();
    }
    fs::remove_file(host_path(&b, "question-released")).unwrap();
    // Legacy identity was allocated from the retained owner marker. Move
    // that publication into a group before rendering the released notice.
    merge(&cfg, "legacy release", &["b".into()]).unwrap();
    check("b", due);
}

#[test]
fn refreshing_notice_policy_keeps_policy_shaped_original_question_text() {
    let (dir, cfg) = fixture();
    asks(&cfg, "a", Some("subject"), false);
    let a = Job::new(&cfg, "a");
    let mut quoted = load(&a).unwrap().unwrap();
    quoted.due -= 3600;
    let original = format!("Is this wording accurate?{}", quoted.notice());
    fs::write(
        a.turn("1", "jsonl"),
        serde_json::json!({"type":"result", "result":original, "is_error":false}).to_string(),
    )
    .unwrap();
    fs::write(host_path(&a, "question-text"), &original).unwrap();
    // New, retained, and both earlier plain formats preserve all the text.
    for mode in ["fresh", "resend", "legacy", "legacy-policy"] {
        if mode.starts_with("legacy") {
            let path = a.turn("1", "question");
            let cached: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let mut text = cached["text"].as_str().unwrap().to_owned();
            if mode == "legacy-policy" {
                // The old generated paragraph's default can itself quote
                // policy; all of it belongs after the complete original.
                quoted.recommendation.default.push_str(&quoted.notice());
                text.push_str(&quoted.notice());
            }
            fs::write(path, text).unwrap();
        }
        notified(&cfg, &["a"]);
        let body = last_ensure(&dir, "proj-question-a-1");
        assert!(
            body.contains(&original),
            "original text was removed: {body}"
        );
        assert_eq!(body.matches("Without an answer by").count(), 2, "{body}");
    }
}

#[test]
fn a_damaged_structured_notice_retries_without_sending_its_serialized_state() {
    let (dir, cfg) = fixture();
    asks(&cfg, "a", None, false);
    notified(&cfg, &["a"]);
    let a = Job::new(&cfg, "a");
    let path = a.turn("1", "question");
    let cached = fs::read(&path).unwrap();
    let before = calls(&dir);
    fs::write(&path, "{damaged").unwrap();
    assert!(crate::question::notify(&cfg, &a).is_err());
    assert_eq!(calls(&dir), before);
    fs::write(&path, cached).unwrap();
    notified(&cfg, &["a"]);
    assert!(last_ensure(&dir, "proj-question-a-1").contains("May a edit the profile?"));
}

#[test]
fn only_a_policy_paragraph_as_notice_writes_it_is_stripped() {
    let q = Question {
        pauses: Default::default(),
        id: None,
        recommendation: Recommendation {
            default: "leave it".into(),
            one_way: false,
            about: None,
        },
        due: 1_800_000_000,
    };
    let body = "Job j needs your answer.\n\nWithout an answer by then, I'd rather wait.";
    assert_eq!(without_policy(body), body);
    assert_eq!(without_policy(&format!("{body}{}", q.notice())), body);
    let mut late = q;
    late.due = i64::MAX;
    assert_eq!(without_policy(&format!("{body}{}", late.notice())), body);
}
