// The hold's two records, read as one: no read finds neither while a hold
// stands through a change of cause, and two records left side by side (a
// crash between writing one and removing the other, or an earlier binary's
// usage record written beside a login hold) are reconciled, never one
// picked and the other's refusal dropped.

use super::*;

const SAID: &str = "Failed to authenticate: OAuth session expired and could not be refreshed";

/// A usage record as an earlier binary writes it, beside whatever's kept.
fn legacy(cfg: &Config, at: i64, last: i64, back: Option<i64>) {
    let text = json!({
        "until": now() + 7200, "said": "weekly limit", "job": "old-1",
        "at": at, "back": back, "told": at, "last": last,
    });
    fs::write(cfg.jobs.join(FILE), format!("{text}\n")).unwrap();
}

/// What `name` holds, to put back as a crash would have left it.
fn text(cfg: &Config, name: &str) -> String {
    fs::read_to_string(cfg.jobs.join(name)).unwrap()
}

/// A read whose file reads are done through `between`, called with each
/// read's number before it reads: what it changes lands between reads.
fn read_with(cfg: &Config, mut between: impl FnMut(usize)) -> Option<Spent> {
    let mut n = 0;
    crate::usage::records::snapshot(cfg, |name| {
        between(n);
        n += 1;
        Ok(fs::read_to_string(cfg.jobs.join(name)).ok())
    })
    .unwrap()
}

/// A change of cause already under way when a read starts: its generation
/// moved before the read's first look, then `name` written whole (`text`)
/// and the other record removed between the usage record's two reads.
fn under_way(cfg: &Config, name: &str, text: &str) -> Option<Spent> {
    let other = if name == FILE { LOGIN_FILE } else { FILE };
    read_with(cfg, |n| match n {
        0 => {
            let g = cfg.jobs.join(crate::usage::records::GENERATION);
            let at = fs::read_to_string(&g).map_or(0, |t| t.trim().parse::<u64>().unwrap());
            fs::write(&g, format!("{}\n", at + 1)).unwrap();
        }
        2 => {
            fs::write(cfg.jobs.join(name), text).unwrap();
            fs::remove_file(cfg.jobs.join(other)).unwrap();
        }
        _ => {}
    })
}

#[test]
fn a_read_across_a_change_of_cause_still_finds_the_hold() {
    // A usage limit replaces the login hold between the read of the usage
    // record (not written yet) and the login record's (already removed).
    let (_d, cfg) = rig();
    let t = now();
    let (_e, other) = rig();
    record(&other, "web-2", Some(t + 7200), "weekly limit", t + 1).unwrap();
    let usage = text(&other, FILE);
    login(&cfg, "web-1", SAID, t).unwrap();
    let s = under_way(&cfg, FILE, &usage);
    assert!(
        s.as_ref().is_some_and(|s| !s.login && s.holds(now())),
        "{s:?}"
    );

    // And the other way: a login record written as the usage record goes.
    let (_d, cfg) = rig();
    login(&cfg, "web-1", SAID, t).unwrap();
    let held = text(&cfg, LOGIN_FILE);
    record(&cfg, "web-2", Some(t + 7200), "weekly limit", t + 1).unwrap();
    let s = under_way(&cfg, LOGIN_FILE, &held);
    assert!(s.as_ref().is_some_and(|s| s.holds(now())), "{s:?}");
}

#[test]
fn a_read_across_two_changes_still_finds_the_hold() {
    // The usage record is missing at both its reads, but only because one
    // change wrote it (removing the login record before that was read) and
    // the next replaced it: the generation tells.
    let (_d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t).unwrap();
    let s = read_with(&cfg, |n| match n {
        2 => {
            let mut usage = kept(&cfg).unwrap();
            usage.login = false;
            keep(&cfg, &usage).unwrap();
        }
        3 => {
            let mut l = kept(&cfg).unwrap();
            l.login = true;
            keep(&cfg, &l).unwrap();
        }
        _ => {}
    });
    assert!(s.is_some_and(|s| s.login && s.holds(now())));
}

#[test]
fn a_delayed_usage_record_keeps_the_later_login_refusal() {
    // The finding's case: a login hold first refused at T and again at
    // T+4, and an earlier binary's usage refusal from T+2 written after.
    let (_d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t).unwrap();
    login(&cfg, "web-2", SAID, t + 4).unwrap();
    legacy(&cfg, t + 2, t + 2, None);
    let s = out(&cfg).unwrap();
    assert!(!s.login, "the usage limit's cause wins: {s:?}");
    assert_eq!(s.refused(), t + 4);
    assert_eq!(s.until, now() + 7200);
    // A meter requested at T+3 is before the login's refusal: nothing ends,
    // and the login record isn't removed.
    release(&cfg, Some(t + 3), true);
    assert!(out(&cfg).is_some());
    assert!(cfg.jobs.join(LOGIN_FILE).exists());
    // One requested since ends the hold, and nothing comes back after it.
    let held = text(&cfg, LOGIN_FILE);
    release(&cfg, Some(t + 5), true);
    assert_eq!(out(&cfg), None);
    assert!(!cfg.jobs.join(LOGIN_FILE).exists());
    assert!(kept(&cfg).unwrap().back.is_some());
    // A crash before the login record went leaves it ended too: the meter's
    // release proved past every refusal both records held.
    fs::write(cfg.jobs.join(LOGIN_FILE), &held).unwrap();
    assert_eq!(out(&cfg), None);
}

#[test]
fn a_login_record_a_crash_left_beside_the_usage_record_it_replaced_holds() {
    let (_d, cfg) = rig();
    let t = now();
    record(&cfg, "web-2", Some(t + 7200), "weekly limit", t).unwrap();
    back(&cfg, Some(t + 1));
    let released = text(&cfg, FILE);
    login(&cfg, "web-1", SAID, t + 2).unwrap();
    fs::write(cfg.jobs.join(FILE), &released).unwrap();
    let s = out(&cfg).unwrap();
    assert!(s.login && s.refused() == t + 2, "{s:?}");
    // Settling the released record it replaced doesn't remove it.
    tend(&cfg);
    assert!(out(&cfg).is_some_and(|s| s.login));
    // Nor does an earlier binary rewriting that record (its notice sent
    // again): it ended before the login was refused.
    let mut rewritten: serde_json::Value = serde_json::from_str(&released).unwrap();
    rewritten["told"] = json!(t + 3);
    fs::write(cfg.jobs.join(FILE), rewritten.to_string()).unwrap();
    assert!(out(&cfg).is_some_and(|s| s.login && s.refused() == t + 2));
    // A run since ends it, and both records with it.
    back(&cfg, Some(t + 3));
    assert_eq!(out(&cfg), None);
    assert!(!cfg.jobs.join(FILE).exists());
}

#[test]
fn a_usage_record_a_crash_left_beside_the_login_record_it_replaced_stands() {
    let (_d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t + 1).unwrap();
    let held = text(&cfg, LOGIN_FILE);
    record(&cfg, "web-2", Some(t + 7200), "weekly limit", t).unwrap();
    fs::write(cfg.jobs.join(LOGIN_FILE), &held).unwrap();
    let s = out(&cfg).unwrap();
    assert!(!s.login && s.job == "web-2" && s.until == t + 7200, "{s:?}");
    assert_eq!(s.refused(), t + 1);
    back(&cfg, Some(t + 1));
    assert!(out(&cfg).is_some());
    back(&cfg, Some(t + 2));
    assert_eq!(out(&cfg), None);
    assert!(!cfg.jobs.join(LOGIN_FILE).exists());
}

#[test]
fn an_ended_usage_record_ends_only_a_login_its_run_started_after() {
    // An earlier binary's run released its usage record, last refused at
    // T+2, when it finished at T+5: it started after T+2, maybe at T+3, so
    // a login refused at T+4 still holds, though it was before the release.
    let (_d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t + 4).unwrap();
    legacy(&cfg, t + 2, t + 2, Some(t + 5));
    assert!(out(&cfg).is_some_and(|s| s.login && s.refused() == t + 4));
    // proj-watch's look meanwhile doesn't end it either.
    tend(&cfg);
    assert!(out(&cfg).is_some_and(|s| s.login && s.refused() == t + 4));
    // A run of this binary's that started by T+4 proves nothing; one since
    // ends it, and both records go to the one it ended.
    back(&cfg, Some(t + 4));
    assert!(out(&cfg).is_some());
    back(&cfg, Some(t + 5));
    assert_eq!(out(&cfg), None);
    assert!(!cfg.jobs.join(FILE).exists());

    // A login refused when the usage record last was, or before, is one
    // that run proved since.
    for refused in [t + 1, t + 2] {
        let (_d, cfg) = rig();
        login(&cfg, "web-1", SAID, refused).unwrap();
        legacy(&cfg, t + 2, t + 2, Some(t + 5));
        assert_eq!(out(&cfg), None, "login refused at {}", refused - t);
        assert_eq!(kept(&cfg).unwrap().release(), t + 5);
    }
}

#[test]
fn an_ended_login_record_ends_a_usage_record_rewritten_since() {
    // A run this binary started after T+3 ended the login hold, and a
    // crash left the usage record it had taken in; an earlier binary sends
    // that one's notice again, so it's now the later written. It was
    // refused at T+1, which the run proved since: nothing holds.
    let (_d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t + 3).unwrap();
    back(&cfg, Some(t + 4));
    assert!(kept(&cfg).is_some_and(|s| s.login && s.back.is_some()));
    legacy(&cfg, t + 1, t + 1, None);
    assert_eq!(out(&cfg), None);
    assert!(kept(&cfg).is_some_and(|s| s.login));
    // A usage refusal since the login's last holds again.
    legacy(&cfg, t + 5, t + 5, None);
    assert!(out(&cfg).is_some_and(|s| !s.login && s.refused() == t + 5));
}

#[test]
fn two_ended_records_are_the_one_released_last() {
    // The login hold was released last; an earlier binary's rewrite of an
    // usage record released before (its close sent again) doesn't move the
    // release back.
    let (_d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t).unwrap();
    back(&cfg, Some(t + 1));
    let released = kept(&cfg).unwrap().release();
    legacy(&cfg, t - 200, t - 200, Some(t - 100));
    let s = kept(&cfg).unwrap();
    assert!(s.login && s.release() == released, "{s:?}");
    // And one the earlier binary released after stands, written either way.
    legacy(&cfg, t - 200, t - 200, Some(released + 50));
    assert_eq!(kept(&cfg).unwrap().release(), released + 50);
}

#[test]
fn same_second_refusals_of_both_causes_are_one_hold() {
    let (_d, cfg) = rig();
    let t = now();
    login(&cfg, "web-1", SAID, t).unwrap();
    legacy(&cfg, t, t, None);
    let s = out(&cfg).unwrap();
    assert!(!s.login && s.refused() == t, "{s:?}");
    // A run started in that second proves nothing.
    back(&cfg, Some(t));
    assert!(out(&cfg).is_some());
    back(&cfg, Some(t + 1));
    assert_eq!(out(&cfg), None);
}

#[test]
fn an_owner_hold_stands_beside_any_login_record() {
    let (_d, cfg) = rig();
    hold(&cfg, now() + 600, "owner pause").unwrap();
    let owner = text(&cfg, FILE);
    let mut l = kept(&cfg).unwrap();
    l.owner = false;
    l.login = true;
    l.job = "web-1".into();
    keep(&cfg, &l).unwrap();
    fs::write(cfg.jobs.join(FILE), &owner).unwrap();
    assert!(out(&cfg).is_some_and(|s| s.owner && !s.login));
    // A run can't end it.
    back(&cfg, Some(now() + 1));
    assert!(out(&cfg).is_some_and(|s| s.owner));
    // The owner can, and the login record refused before it goes with it,
    // even where a crash left it.
    let left = text(&cfg, LOGIN_FILE);
    back(&cfg, None);
    assert_eq!(out(&cfg), None);
    fs::write(cfg.jobs.join(LOGIN_FILE), &left).unwrap();
    assert_eq!(out(&cfg), None);
}
