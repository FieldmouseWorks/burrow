//! The registry is the authority on a shared question's policy: an error in
//! it is never read as no question, every decision on policy reads it under
//! its lock, an accepted answer retires a member at once, a pause gives back
//! its time through it, and every notice renders it as it is now.
use super::*;

fn registry(cfg: &Config) -> std::path::PathBuf {
    control_root(cfg).join(".about/state.json")
}

fn edit_group(cfg: &Config, key: &str, edit: impl FnOnce(&mut serde_json::Value)) {
    let mut state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(registry(cfg)).unwrap()).unwrap();
    edit(&mut state[key]);
    fs::write(registry(cfg), state.to_string()).unwrap();
}

fn group_due(cfg: &Config, key: &str) -> i64 {
    let state: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(registry(cfg)).unwrap()).unwrap();
    state[key]["due"].as_i64().unwrap()
}

/// An accepted answer to `id`'s question whose cleanup never ran.
fn accepted_unapplied(cfg: &Config, id: &str, named: bool) {
    let job = Job::new(cfg, id);
    let question = named.then(|| about::question_id(&job));
    fs::write(
        host_path(&job, "resume-2"),
        serde_json::json!({
            "through": 1, "answered": true, "clear": true, "deadline": false,
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
fn an_accepted_answer_retires_its_member_before_its_cleanup_runs() {
    for named in [true, false] {
        let (_dir, cfg) = fixture();
        asks(&cfg, "old", Some("profile"), true);
        let past = crate::now() - 1;
        edit_group(&cfg, "profile", |g| g["due"] = past.into());
        let old = Job::new(&cfg, "old");
        // Answered and accepted; the process died before its cleanup.
        accepted_unapplied(&cfg, "old", named);
        assert!(host_path(&old, "question").exists());
        assert!(load(&old).unwrap().is_none(), "named {named}");
        assert!(open(&old).unwrap().is_none(), "named {named}");
        // A new question on the subject starts a fresh group: its own
        // deadline and requirement, not the retired one's, and it leads.
        asks(&cfg, "new", Some("profile"), false);
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

fn park(cfg: &Config, id: &str, at: i64, due: Option<i64>) -> crate::policy::Park {
    let rec = crate::policy::Park {
        state: "done".into(),
        turn: "1".into(),
        first: false,
        at,
        due,
    };
    fs::create_dir_all(
        crate::policy::path(&Job::new(cfg, id), "park")
            .parent()
            .unwrap(),
    )
    .unwrap();
    fs::write(
        crate::policy::path(&Job::new(cfg, id), "park"),
        serde_json::to_string(&rec).unwrap(),
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
        assert!(near(rec.due.unwrap(), due + 600), "{id}");
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
    // A park from before records its deadline the first time, so a retry
    // extends that one rather than the one already extended.
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
    let legacy = format!("{}{}", fs::read_to_string(&cached).unwrap(), first.notice());
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
fn only_a_policy_paragraph_as_notice_writes_it_is_stripped() {
    let q = Question {
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
