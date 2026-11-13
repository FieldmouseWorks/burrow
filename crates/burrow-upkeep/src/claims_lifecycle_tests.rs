//! Which jobs `claims` retires, which turns' answers may go up, and what
//! counts as waking a job awaiting the PR.

use super::*;

/// A job `claims` looks at, with its folder `dir` in `jobs`.
fn folder(jobs: &std::path::Path) -> PathBuf {
    let dir = jobs.join("job-1");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A look with the job's states and holds as given, whatever follows `l`.
fn looked(hub: &mut Hub, l: &Look, state: &str, held: &[&str]) -> Seen {
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg = config(cfg_dir.path());
    let mut states = States::new(state);
    states
        .held
        .insert("job-1".into(), held.iter().map(|h| h.to_string()).collect());
    let (_, got) = look(&cfg, hub, &mut states, 0.0, |p| Ok(claims(p, l)));
    got.unwrap()
}

// A stop proj refused leaves the job kept, retiring, and named again on
// every pass, until proj says it's over: no pass takes it for gone.
#[test]
fn a_stale_job_stays_retiring_until_proj_confirms_it_is_over() {
    let jobs = tempfile::tempdir().unwrap();
    folder(jobs.path());
    let mut l = at(THEIRS, jobs.path(), true);
    let mut hub = Hub::default();
    hub.on("GET", ACTIVITY, reply(200, pushes("sam", THEIRS)));
    let s = looked(&mut hub, &l, "running", &[]);
    assert_eq!(s.stop.len(), 1);
    assert!(!s.clear && s.answering["job-1"].retiring);
    // The stop failed: the next passes name it again, and it blocks.
    l.answering = s.answering;
    l.pushed_by = s.pushed_by;
    for held in [vec![], vec!["await sam/up#1"]] {
        let s = looked(&mut Hub::default(), &l, "running", &held);
        assert_eq!(s.stop.len(), 1, "{held:?}");
        assert!(!s.clear && s.answering["job-1"].retiring, "{held:?}");
        l.answering = s.answering;
    }
    // Stopped, holding nothing: retired, and the branch is clear.
    let s = looked(&mut Hub::default(), &l, "stopped", &[]);
    assert!(s.stop.is_empty() && s.answering.is_empty() && s.clear);
}

// Several jobs retire at once: the branch is clear only when every one is
// confirmed, and one that isn't blocks it.
#[test]
fn every_retiring_job_is_confirmed_before_the_branch_is_clear() {
    let jobs = tempfile::tempdir().unwrap();
    for j in ["job-1", "job-2"] {
        std::fs::create_dir_all(jobs.path().join(j)).unwrap();
        std::fs::write(jobs.path().join(j).join("await"), "SAM/R#5\t0\t{}\n").unwrap();
        std::fs::write(jobs.path().join(j).join("turns"), "1").unwrap();
    }
    let mut l = at(HANDED, jobs.path(), true);
    l.answering.insert("job-2".into(), handing());
    l.retire = true;
    let cfg_dir = tempfile::tempdir().unwrap();
    let cfg = config(cfg_dir.path());
    let mut states = States::new("done");
    states.record = Some(jobs.path().to_path_buf());
    let (_, got) = look(&cfg, &mut Hub::default(), &mut states, 0.0, |p| {
        Ok(claims(p, &l))
    });
    let s = got.unwrap();
    assert_eq!(s.stop.len(), 2);
    assert!(!s.clear && s.answering.values().all(|r| r.retiring && r.owns));
    // job-1 is over; job-2 still isn't.
    l.retire = false;
    l.answering = s.answering;
    states.of.insert("job-1".into(), "stopped".into());
    states.held.insert("job-1".into(), vec![]);
    states
        .held
        .insert("job-2".into(), vec!["await SAM/R#5".into()]);
    let (_, got) = look(&cfg, &mut Hub::default(), &mut states, 0.0, |p| {
        Ok(claims(p, &l))
    });
    let s = got.unwrap();
    assert_eq!(s.answering.keys().collect::<Vec<_>>(), ["job-2"]);
    assert!(!s.clear && s.stop.len() == 1);
}

// A job resumed by what went up isn't retired, and keeps the branch
// reserved; one whose turn left nothing new is.
#[test]
fn retiring_leaves_a_resumed_job_and_blocks_on_it() {
    let jobs = tempfile::tempdir().unwrap();
    let dir = folder(jobs.path());
    std::fs::write(dir.join("await"), "SAM/R#5\t0\t{}\n").unwrap();
    std::fs::write(dir.join("turns"), "1").unwrap();
    let mut l = at(HANDED, jobs.path(), true);
    l.answering.get_mut("job-1").unwrap().woke_at = Some(T);
    l.retire = true;
    let s = looked(&mut Hub::default(), &l, "done", &["await SAM/R#5"]);
    let kept = &s.answering["job-1"];
    assert!(kept.resumes && !kept.retiring && s.stop.is_empty() && !s.clear);
    l.answering.get_mut("job-1").unwrap().woke_at = None;
    let s = looked(&mut Hub::default(), &l, "done", &["await SAM/R#5"]);
    assert!(s.answering["job-1"].retiring && s.stop.len() == 1 && !s.clear);
}

/// Turn `n`'s kept answers, with what it held.
fn kept_turn(dir: &std::path::Path, n: u32, held: &str, answers: Value) {
    std::fs::write(dir.join(format!("{n}.answers")), answers.to_string()).unwrap();
    std::fs::write(dir.join(format!("{n}.holds")), held).unwrap();
}

// Whether a turn's answers may go up is its own: a turn that held a
// question or something upstream leaves them for the turn that replaces
// them, the others are taken whatever the latest turn is doing.
#[test]
fn each_turn_is_eligible_by_what_it_held() {
    let jobs = tempfile::tempdir().unwrap();
    let dir = folder(jobs.path());
    std::fs::write(dir.join("turns"), "4").unwrap();
    let comment = |c: &str| json!({"pushed": null, "comment": c});
    kept_turn(&dir, 1, "ask", comment("intermediate question"));
    kept_turn(&dir, 2, "await sam/up#3\nawait SAM/R#5", comment("mixed"));
    kept_turn(&dir, 3, "await SAM/R#5", comment("own"));
    kept_turn(&dir, 4, "usage Claude 1", comment("limit"));
    let l = at(HANDED, jobs.path(), true);
    // The latest turn is running or holding something elsewhere: turn 3's
    // are taken all the same.
    for (state, held) in [("running", vec![]), ("done", vec!["await sam/up#3"])] {
        let mut hub = github(HANDED, "Does it.", json!([]));
        let s = looked(&mut hub, &l, state, &held);
        assert_eq!(s.answered.len(), 1, "{state}: {:?}", s.answered);
        assert_eq!(writes(&hub), ["POST repos/sam/r/issues/5/comments"]);
        assert!(s.answering["job-1"].taken.len() == 1 && s.answering["job-1"].owns);
    }
}

// A turn that held a question and one that replaced its answers: only the
// final turn's reply goes up, never the intermediate one.
#[test]
fn an_intermediate_held_reply_is_replaced_by_the_final_one() {
    let jobs = tempfile::tempdir().unwrap();
    let dir = folder(jobs.path());
    std::fs::write(dir.join("turns"), "2").unwrap();
    let reply_on = |text: &str| json!({"pushed": null, "threads": [{"id": "T1", "fixed": false, "reply": text}]});
    kept_turn(&dir, 1, "ask", reply_on("intermediate"));
    kept_turn(&dir, 2, "", reply_on("final"));
    let open = json!([{"id": "T1", "isResolved": false, "comments": {"nodes": [{"body": "**Codex · P1** x"}]}}]);
    let mut hub = github(HANDED, "Does it.", open);
    let s = looked(&mut hub, &at(HANDED, jobs.path(), true), "done", &[]);
    assert_eq!(s.answered.len(), 1);
    let said: Vec<_> = hub
        .asked
        .iter()
        .filter_map(|(_, _, _, b)| b.as_ref()?["variables"]["b"].as_str().map(String::from))
        .collect();
    assert_eq!(said.len(), 1);
    assert!(said[0].contains("final") && !said[0].contains("intermediate"));
}

// Two pushes between passes: turn 1's answers are for a head the PR has
// left, and turn 2's, on the current head, still go up.
#[test]
fn an_obsolete_turn_does_not_discard_a_newer_one() {
    let jobs = tempfile::tempdir().unwrap();
    let dir = folder(jobs.path());
    std::fs::write(dir.join("turns"), "2").unwrap();
    kept_turn(
        &dir,
        1,
        "await SAM/R#5",
        json!({"pushed": THEIRS, "comment": "one"}),
    );
    kept_turn(&dir, 2, "", json!({"pushed": PUSHED, "comment": "two"}));
    let mut l = at(PUSHED, jobs.path(), true);
    l.answering.get_mut("job-1").unwrap().head = HANDED.into();
    let mut hub = github(PUSHED, "Does it.", json!([]));
    let s = looked(&mut hub, &l, "done", &[]);
    let outcomes: Vec<_> = s.answered.iter().map(|a| a.outcome).collect();
    assert_eq!(outcomes, ["moved", "posted"], "{:?}", s.answered);
    assert_eq!(writes(&hub), ["POST repos/sam/r/issues/5/comments"]);
}

// An earlier no-push answer overtaken by a later push is rejected, and the
// later one still goes up; a closed PR takes neither.
#[test]
fn an_overtaken_no_push_answer_is_rejected_and_the_later_one_posts() {
    let jobs = tempfile::tempdir().unwrap();
    let dir = folder(jobs.path());
    std::fs::write(dir.join("turns"), "2").unwrap();
    kept_turn(
        &dir,
        1,
        "await SAM/R#5",
        json!({"pushed": null, "comment": "one"}),
    );
    kept_turn(&dir, 2, "", json!({"pushed": PUSHED, "comment": "two"}));
    let l = at(PUSHED, jobs.path(), true);
    let mut hub = github(PUSHED, "Does it.", json!([]));
    let s = looked(&mut hub, &l, "done", &[]);
    let outcomes: Vec<_> = s.answered.iter().map(|a| a.outcome).collect();
    assert_eq!(outcomes, ["moved", "posted"], "{:?}", s.answered);
    assert!(s.answering.is_empty());
}

// A write whose answer was lost may have woken the job, so it counts; a
// reply on a thread counts as a comment does; one found already up doesn't.
#[test]
fn what_may_have_woken_a_job_is_counted_against_its_await() {
    let run = |hub: &mut Hub, answers: Value, since: &str| {
        let jobs = tempfile::tempdir().unwrap();
        let dir = folder(jobs.path());
        std::fs::write(dir.join("turns"), "1").unwrap();
        std::fs::write(dir.join("await"), format!("SAM/R#5\t{since}\t{{}}\n")).unwrap();
        std::fs::write(dir.join("answers.json"), answers.to_string()).unwrap();
        let s = looked(
            hub,
            &at(HANDED, jobs.path(), true),
            "done",
            &["await SAM/R#5"],
        );
        s.answering["job-1"].clone()
    };
    let reply_only =
        json!({"pushed": null, "threads": [{"id": "T1", "fixed": false, "reply": "No."}]});
    let open = json!([{"id": "T1", "isResolved": false, "comments": {"nodes": [{"body": "**Codex · P1** x"}]}}]);
    // A reply on a review thread (fingerprint: review_comments) wakes it.
    let kept = run(
        &mut github(HANDED, "Does it.", open.clone()),
        reply_only.clone(),
        "0",
    );
    assert!(kept.resumes);
    // Written before this await began (a delayed snapshot of an older
    // turn), it woke nothing.
    let kept = run(
        &mut github(HANDED, "Does it.", open.clone()),
        reply_only.clone(),
        "1800000001",
    );
    assert!(!kept.resumes && kept.woke_at == Some(T));
    // The comment's answer lost: the write may have gone through.
    let mut hub = github(HANDED, "Does it.", json!([]));
    hub.on(
        "POST",
        "repos/sam/r/issues/5/comments$",
        reply(502, json!({})),
    );
    let kept = run(&mut hub, json!({"pushed": null, "comment": "Hm."}), "0");
    assert!(kept.resumes && kept.taken.is_empty());
}
