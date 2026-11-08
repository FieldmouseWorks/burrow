//! A pass: security fixes, one repo's pass, and what it can't do.

use super::*;

#[test]
fn github_recovers_after_502() {
    let m = Machine::new();
    m.set("once-502", "");
    m.prs(&[red(1)]);
    let p = m.pass();
    assert!(p.task().contains("#1 "));
    let calls = fs::read_to_string(m.path("fake/api-log")).unwrap();
    assert_eq!(
        calls
            .lines()
            .filter(|l| l.starts_with("gh api -i -X GET user "))
            .count(),
        2
    );
}

#[test]
fn a_security_fix_merges_itself_and_is_the_owners_a_day_on() {
    // No job is asked for a verdict on one, a major too, and it's never
    // listed as held.
    let m = Machine::new();
    m.prs(&[pr_with(5, &["major", "security"], "SUCCESS")]);
    let v = m.verdict(5, "held", "main", "me");
    m.comments(5, &[v]);
    let p = m.pass();
    assert!(p.jobs.is_empty() && p.majors().0 == "close");
    assert_eq!(p.notice("security").0, "close");
    // A red one goes to a job once a day, not once a head.
    let m = Machine::new();
    let mut old = pr_with(5, &["security"], "FAILURE");
    old["createdAt"] = "2000-01-01T00:00:00Z".into();
    m.prs(&[old.clone()]);
    let p = m.pass();
    assert!(p.task().contains("#5 "));
    let today = jiff::Zoned::now().strftime("%Y-%m-%d").to_string();
    assert_eq!(m.key(5, ""), format!("{} {today} red\n", sha(5)));
    let stuck = "Renovate opened these security fixes over a day ago and they haven't merged. A red one goes to a Claude job every day; if the same one keeps failing, the cause is probably outside the repo (its CI runner, a broken default branch):\n- Update foo5 to v2: https://github.com/sam/r/pull/5";
    assert_eq!(
        p.notice("security"),
        ("open", format!("{stuck} (its CI fails)").as_str())
    );
    assert!(m.pass().jobs.is_empty());
    m.keep(5, "", &format!("{} 2000-01-01 red\n", sha(5)));
    assert!(m.pass().task().contains("#5 "));
    // Whatever holds it, it's listed.
    let mut green = old.clone();
    green["statusCheckRollup"][0]["conclusion"] = "SUCCESS".into();
    let mut untested = old.clone();
    untested["statusCheckRollup"] = json!([]);
    for (p, why) in [
        (green, "its CI is green and it still hasn't merged"),
        (untested, "nothing has tested it"),
        (
            with(
                old.clone(),
                json!({"__typename": "StatusContext", "context": "x", "state": "PENDING"}),
            ),
            "its CI fails",
        ),
        (ci(5, &[("IN_PROGRESS", "")]), ""),
    ] {
        let m = Machine::new();
        let mut p = p;
        p["labels"] = json!([{"name": "security"}]);
        p["createdAt"] = "2000-01-01T00:00:00Z".into();
        m.prs(&[p]);
        let got = m.pass();
        let (verb, body) = got.notice("security");
        let why = if why.is_empty() {
            "its CI hasn't finished"
        } else {
            why
        };
        assert_eq!((verb, body), ("open", format!("{stuck} ({why})").as_str()));
    }
    // Under a day, it isn't.
    let m = Machine::new();
    let mut new = old;
    let hour_ago = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    new["createdAt"] = hour_ago.to_string().into();
    m.prs(&[new]);
    assert_eq!(m.pass().notice("security").0, "close");
}

#[test]
fn one_repos_sweep_is_that_repos_and_leaves_the_notices_to_a_full_one() {
    let two = || {
        let m = Machine::new();
        m.project("q", "git@github.com:sam/q");
        m.prs(&[pr(1)]);
        m
    };
    assert_eq!(two().pass().projects(), ["q", "r"]);
    // Named as GitHub spells it, whatever the clone's remote says.
    let p = two().sweep(&["Sam/R"]);
    assert_eq!((p.code, p.err.as_str()), (Some(0), ""));
    assert_eq!(p.projects(), ["r"]);
    assert!(p.notices.is_empty(), "{:#?}", p.notices);
    // A hidden folder counts when it's named for its repo, and only then;
    // one whose origin names no OWNER/REPO, or that has none, doesn't.
    let hidden = || {
        let m = two();
        m.project(".github", "https://github.com/sam/.github.git");
        m.project(".mirror", "https://github.com/sam/mirrored.git");
        m.project("local", "local");
        fs::create_dir(m.path("projects/bare")).unwrap();
        m
    };
    assert_eq!(hidden().pass().projects(), [".github", "q", "r"]);
    // A repo's own PRs only.
    let m = hidden();
    m.set("prs-sam_q", "[]");
    assert_eq!(m.pass().projects(), [".github", "r"]);
}

#[test]
fn what_it_cant_do_it_says_and_fails_the_pass() {
    // A notice that didn't go: the other still does.
    let m = Machine::new();
    m.set("notify-fails", "");
    let p = m.sweep(&[]);
    assert_eq!(p.code, Some(1));
    assert_eq!(
        p.err,
        format!(
            "renovate-sweep: {0} close: exit status: 4\nrenovate-sweep: {0} close: exit status: 4\n",
            bin().join("notify").display()
        )
    );
    assert_eq!(p.notices.len(), 2);
    // Limits it can't read: nothing is swept.
    let m = Machine::new();
    m.prs(&[pr(1)]);
    m.set("limits", "3 21600 3\n");
    let p = m.sweep(&[]);
    assert_eq!(p.code, Some(1));
    assert_eq!(
        p.err,
        format!(
            "renovate-sweep: {} --limits: exit status: 0, saying \"3 21600 3\"\n",
            bin().join("lock-wait").display()
        )
    );
    assert!(p.jobs.is_empty() && p.notices.is_empty());
    // Its first line is them.
    m.set("limits", "3 21600 3\n600\n");
    let p = m.sweep(&[]);
    assert_eq!(p.code, Some(1));
    assert!(p.jobs.is_empty() && p.notices.is_empty());
    // A key it can't keep: what it's for goes to no job, this pass or the
    // next, while the others still do.
    let m = Machine::new();
    m.prs(&[pr(1), red(2)]);
    fs::create_dir_all(m.state("sam_r-1/x")).unwrap();
    for _ in 0..2 {
        let p = m.sweep(&[]);
        assert_eq!(p.code, Some(1));
        assert!(
            p.err.starts_with(&format!(
                "renovate-sweep: {}: ",
                m.state("sam_r-1").display()
            )),
            "{}",
            p.err
        );
        assert_eq!(p.jobs.len(), 1);
        assert!(!p.task().contains("#1 ") && p.task().contains("#2 "));
        fs::remove_file(m.state("sam_r-2")).unwrap();
    }
}

#[test]
fn sweeps_side_by_side_hand_each_head_to_one_job() {
    let finish = |c: Child| {
        let out = c.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    };
    let jobs = |m: &Machine| {
        fs::read_dir(m.path("fake"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("job-")
            })
            .count()
    };
    let until = |what: &Path| {
        let by = SystemTime::now() + Duration::from_secs(30);
        while !what.exists() {
            assert!(SystemTime::now() < by, "no {}", what.display());
            thread::sleep(Duration::from_millis(20));
        }
    };
    // A full sweep that has decided on PR 1 and is still reading PR 2, or
    // that is starting their job: a sweep of the repo (as another clone
    // spells it) waits for it, then finds them handed over.
    for blocked in ["gh-blocked", "job-0"] {
        let m = Machine::new();
        m.prs(&[red(1), pr(2)]);
        match blocked {
            "gh-blocked" => m.set("gh-blocks", "2"),
            _ => m.set("proj-blocks", ""),
        }
        let full = m.start(&[]);
        until(&m.path("fake").join(blocked));
        let one = m.start(&["Sam/R"]);
        thread::sleep(Duration::from_millis(500));
        assert_eq!(jobs(&m), usize::from(blocked == "job-0"));
        m.set("gh-go", "");
        m.unset("proj-blocks");
        finish(full);
        finish(one);
        assert_eq!(jobs(&m), 1, "{blocked}");
        let job = fs::read_to_string(m.path("fake/job-0")).unwrap();
        assert!(job.contains("#1 ") && job.contains("#2 "));
        // The job doesn't hold the lock: a job outliving its sweep holds
        // up no other.
        assert!(!m.path("fake/proj-held-lock").exists());
    }
}

#[test]
fn a_usage_or_a_config_it_cant_take_exits_2_having_run_nothing() {
    let m = Machine::new();
    m.prs(&[pr(1)]);
    for words in [
        &["sam"][..],
        &["sam/r", "now"],
        &["sam/r/x"],
        &[""],
        &["/r"],
    ] {
        let p = m.sweep(words);
        assert_eq!(p.code, Some(2), "{words:?}");
        assert_eq!(p.err, "usage: renovate-sweep [OWNER/REPO]\n");
        assert!(p.jobs.is_empty() && p.notices.is_empty() && p.log.is_empty());
    }
    let none = Command::new(SWEEP).output().unwrap();
    assert_eq!(none.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&none.stderr),
        "usage: burrow-renovate-sweep CONFIG [OWNER/REPO]\n"
    );
    let path = m.path("bad.json");
    fs::write(&path, r#"{"name": "renovate-sweep"}"#).unwrap();
    let bad = Command::new(SWEEP).arg(&path).output().unwrap();
    assert_eq!(bad.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&bad.stderr).starts_with(&format!(
            "burrow-renovate-sweep: {}: missing field",
            path.display()
        )),
        "{}",
        String::from_utf8_lossy(&bad.stderr)
    );
}
