//! A pass: security fixes, one repo's pass, and what it can't do.

use super::*;

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
    // A key it can't keep.
    let m = Machine::new();
    m.prs(&[pr(1)]);
    fs::create_dir_all(m.state("sam_r-1/x")).unwrap();
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
