use super::*;

const M: i64 = 1_000_000;

fn day(s: &str) -> Date {
    Day::parse(s).unwrap().0
}

fn at(s: &str) -> i64 {
    s.parse::<Timestamp>().unwrap().as_second()
}

/// $100 as of 2026-11-20 00:00Z, renewing 2026-11-30 (10 days) to $200,
/// with a $5 reserve.
fn credit() -> Credit {
    Credit {
        budget_usd: Cents(10_000),
        renews: Day(day("2026-11-30")),
        monthly_usd: Cents(20_000),
        reserve_usd: Cents(500),
        since: at("2026-11-20T00:00:00Z"),
    }
}

fn spend(when: &str, usd_micros: i64) -> Spend {
    Spend {
        at: at(when),
        micros: usd_micros,
    }
}

#[test]
fn room_needs_spent_plus_reserve_strictly_under_the_budget() {
    let now = at("2026-11-29T12:00:00Z"); // the last day: allowance is all of it
    let edge = [spend("2026-11-21T00:00:00Z", 95 * M)];
    let m = Meter::at(&credit(), now, &edge).unwrap();
    assert_eq!(
        (m.spent, m.reserve, m.cycle.budget),
        (95 * M, 5 * M, 100 * M)
    );
    assert!(!m.room(), "spent + reserve == budget is no room");
    assert_eq!(m.short().as_deref(), Some("spent down to the reserve"));
    let under = [spend("2026-11-21T00:00:00Z", 95 * M - 1)];
    let mut m = Meter::at(&credit(), now, &under).unwrap();
    assert!(m.room());
    // A turn whose cost isn't known yet takes the room, and says so.
    m.pending = Some("turn 3 is pending".into());
    assert!(!m.room());
    assert!(
        m.text()
            .starts_with("claude-api credit: no room (turn 3 is pending)\n")
    );
}

#[test]
fn an_exhausted_budget_has_no_room_and_no_allowance() {
    let now = at("2026-11-25T12:00:00Z");
    let over = [spend("2026-11-21T00:00:00Z", 130 * M)];
    let m = Meter::at(&credit(), now, &over).unwrap();
    assert!(!m.room());
    assert!(m.allowance < 0, "{m:?}");
    assert_eq!(m.remaining(), -30 * M);
    assert!(m.text().contains("remaining -$30.00"), "{}", m.text());
}

#[test]
fn today_spends_below_an_even_share_of_what_is_left() {
    // 2026-11-20: 10 days left, $100: $10 a day.
    let now = at("2026-11-20T18:00:00Z");
    let m = Meter::at(&credit(), now, &[]).unwrap();
    assert_eq!((m.days_left, m.allowance), (10, 10 * M));
    assert!(m.room());
    let at_allowance = [spend("2026-11-20T01:00:00Z", 10 * M)];
    let m = Meter::at(&credit(), now, &at_allowance).unwrap();
    assert_eq!(m.today, 10 * M);
    assert!(!m.room(), "today's spend equal to the allowance is no room");
    assert_eq!(m.short().as_deref(), Some("today's allowance spent"));
    let under = [spend("2026-11-20T01:00:00Z", 10 * M - 1)];
    assert!(Meter::at(&credit(), now, &under).unwrap().room());
    // The next day's allowance counts what's left, not what yesterday took:
    // $90 over 9 days.
    let next = Meter::at(&credit(), at("2026-11-21T00:00:00Z"), &at_allowance).unwrap();
    assert_eq!((next.days_left, next.today, next.allowance), (9, 0, 10 * M));
    assert!(next.room());
}

#[test]
fn what_a_day_leaves_unspent_is_there_later() {
    // Nothing spent for 5 days: $100 over the 5 left is $20 a day.
    let m = Meter::at(&credit(), at("2026-11-25T08:00:00Z"), &[]).unwrap();
    assert_eq!((m.days_left, m.allowance), (5, 20 * M));
    // A turn spent yesterday comes out of the share, not today's spend.
    let spends = [spend("2026-11-24T23:59:59Z", 50 * M)];
    let m = Meter::at(&credit(), at("2026-11-25T08:00:00Z"), &spends).unwrap();
    assert_eq!((m.today, m.allowance), (0, 10 * M));
}

#[test]
fn the_last_day_may_spend_all_but_the_reserve() {
    let now = at("2026-11-29T23:00:00Z");
    let spends = [
        spend("2026-11-22T00:00:00Z", 20 * M),
        spend("2026-11-29T01:00:00Z", 70 * M),
    ];
    let m = Meter::at(&credit(), now, &spends).unwrap();
    assert_eq!(m.days_left, 1);
    assert_eq!(
        m.allowance,
        80 * M,
        "the last day's allowance is what's left"
    );
    assert_eq!(m.today, 70 * M);
    assert!(m.room(), "$90 spent of $100 with $5 kept: room");
    let more = [spends[0], spends[1], spend("2026-11-29T02:00:00Z", 5 * M)];
    assert!(!Meter::at(&credit(), now, &more).unwrap().room());
}

#[test]
fn renewal_discards_the_old_credit_for_the_monthly_amount() {
    let spends = [spend("2026-11-29T23:00:00Z", 99 * M)];
    let before = Meter::at(&credit(), at("2026-11-29T23:59:59Z"), &spends).unwrap();
    assert!(!before.room());
    let m = Meter::at(&credit(), at("2026-11-30T00:00:00Z"), &spends).unwrap();
    assert_eq!(m.cycle.start, at("2026-11-30T00:00:00Z"));
    assert_eq!(m.cycle.expires, at("2026-12-30T00:00:00Z"));
    assert_eq!((m.cycle.budget, m.spent, m.days_left), (200 * M, 0, 30));
    assert!(m.cycle.renewed);
    assert!(m.room());
    assert!(
        m.text().contains("renewed to the monthly amount"),
        "{}",
        m.text()
    );
}

#[test]
fn renewals_missed_while_down_leave_one_month_of_credit() {
    // Down from December to mid-March: three renewals passed unseen.
    let spends = [
        spend("2026-12-01T00:00:00Z", 150 * M),
        spend("2027-02-28T00:00:00Z", 150 * M),
        spend("2027-03-01T00:00:00Z", 3 * M),
    ];
    let m = Meter::at(&credit(), at("2027-03-10T12:00:00Z"), &spends).unwrap();
    assert_eq!(m.cycle.start, at("2027-02-28T00:00:00Z"), "Feb has no 30th");
    assert_eq!(m.cycle.expires, at("2027-03-30T00:00:00Z"));
    assert_eq!(m.cycle.budget, 200 * M, "never the missed months' sum");
    assert_eq!(m.spent, 153 * M);
}

#[test]
fn renewals_keep_their_day_of_the_month() {
    let mut c = credit();
    c.renews = Day(day("2027-01-31"));
    for (now, start, expires) in [
        ("2027-02-27T00:00:00Z", "2027-01-31", "2027-02-28"),
        ("2027-03-01T00:00:00Z", "2027-02-28", "2027-03-31"),
        ("2027-04-15T00:00:00Z", "2027-03-31", "2027-04-30"),
        ("2028-02-29T00:00:00Z", "2028-02-29", "2028-03-31"),
    ] {
        let cycle = c.cycle(at(now)).unwrap();
        assert_eq!(
            (cycle.start, cycle.expires),
            (
                midnight(day(start)).unwrap(),
                midnight(day(expires)).unwrap()
            ),
            "{now}"
        );
    }
}

#[test]
fn turns_before_the_cycle_and_reruns_of_the_meter_count_nothing_twice() {
    let spends = [
        spend("2026-11-19T23:59:59Z", 40 * M), // before since: the balance had it
        spend("2026-11-20T00:00:00Z", 2 * M),
    ];
    let now = at("2026-11-20T10:00:00Z");
    let one = Meter::at(&credit(), now, &spends).unwrap();
    assert_eq!(one.spent, 2 * M);
    assert_eq!(Meter::at(&credit(), now, &spends).unwrap(), one);
}

#[test]
fn a_setting_that_contradicts_itself_has_no_meter() {
    let mut c = credit();
    c.since = midnight(c.renews.0).unwrap();
    assert!(Meter::at(&c, at("2026-11-29T00:00:00Z"), &[]).is_err());
    let negative = [spend("2026-11-21T00:00:00Z", -1)];
    assert!(Meter::at(&credit(), at("2026-11-22T00:00:00Z"), &negative).is_err());
}

#[test]
fn amounts_and_days_have_one_spelling() {
    assert_eq!(Cents::parse("214"), Some(Cents(21400)));
    assert_eq!(Cents::parse("214.5"), Some(Cents(21450)));
    assert_eq!(Cents::parse("214.37"), Some(Cents(21437)));
    for bad in [
        "",
        ".5",
        "5.",
        "1.005",
        "-1",
        "1e3",
        "$5",
        " 5",
        "12345678901",
    ] {
        assert_eq!(Cents::parse(bad), None, "{bad}");
    }
    assert!(Day::parse("2026-12-05").is_some());
    for bad in [
        "2026-12-5",
        "2026-02-30",
        "20261205",
        "2026-12-05T00:00:00Z",
    ] {
        assert!(Day::parse(bad).is_none(), "{bad}");
    }
    assert_eq!(usd(1_200_000), "$1.20");
    assert_eq!(usd(123_456), "$0.123456");
    assert_eq!(usd(-30 * M), "-$30.00");
}

#[test]
fn malformed_or_unreadable_settings_are_errors_and_absence_is_none() {
    assert_eq!(parse(b"{}").unwrap(), None);
    assert!(parse(b"{").is_err());
    let doc = |c: serde_json::Value| serde_json::to_vec(&serde_json::json!({"credit": c})).unwrap();
    let good = serde_json::json!({"budgetUsd": 214.37, "renews": "2026-12-05", "monthlyUsd": 200, "reserveUsd": 5, "since": 1});
    assert_eq!(
        parse(&doc(good.clone())).unwrap().unwrap().budget_usd,
        Cents(21437)
    );
    for (field, bad) in [
        ("budgetUsd", serde_json::json!(1.001)),
        ("renews", serde_json::json!("12/05/2026")),
        ("since", serde_json::json!(-1)),
    ] {
        let mut value = good.clone();
        value[field] = bad;
        assert!(parse(&doc(value)).is_err(), "{field}");
    }
    let mut extra = good;
    extra["note"] = "x".into();
    assert!(parse(&doc(extra)).is_err());

    let tmp = tempfile::tempdir().unwrap();
    let cfg = Config {
        overlay: tmp.path().join("settings/overlay.json"),
        ..Config::default()
    };
    assert_eq!(cfg.credit().unwrap(), None, "no file yet: no credit");
    std::fs::create_dir_all(cfg.credit_file()).unwrap();
    assert!(cfg.credit().is_err(), "a credit file that can't be read");
}

fn words(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_owned).collect()
}

#[test]
fn the_door_sets_it_absolutely_and_keeps_the_balance_reading() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = Config {
        overlay: tmp.path().join("settings/overlay.json"),
        ..Config::default()
    };
    let renews = utc_day(crate::now())
        .unwrap()
        .checked_add(20.days())
        .unwrap()
        .to_string();
    let said = cfg
        .command(&words(&format!("credit 214.37 {renews} 200 5")), "t")
        .unwrap();
    assert!(said.contains("$214.37"), "{said}");
    let set = cfg.credit().unwrap().unwrap();
    assert_eq!(
        (set.budget_usd, set.monthly_usd, set.reserve_usd),
        (Cents(21437), Cents(20000), Cents(500))
    );
    // An earlier reading of the same balance keeps its start through a
    // repeat and a reserve change: what it spent since still counts.
    let earlier = Credit {
        since: set.since - 3 * DAY,
        ..set.clone()
    };
    std::fs::write(
        cfg.credit_file(),
        serde_json::to_vec(&serde_json::json!({"credit": earlier})).unwrap(),
    )
    .unwrap();
    cfg.command(&words(&format!("credit 214.37 {renews} 200 5")), "t")
        .unwrap();
    assert_eq!(cfg.credit().unwrap().unwrap(), earlier);
    cfg.command(&words(&format!("credit 214.37 {renews} 200 10")), "t")
        .unwrap();
    let reserve = cfg.credit().unwrap().unwrap();
    assert_eq!(
        (reserve.since, reserve.reserve_usd),
        (earlier.since, Cents(1000))
    );
    // A new balance is a new reading: it counts from now.
    cfg.command(&words(&format!("credit 150 {renews} 200 10")), "t")
        .unwrap();
    assert!(cfg.credit().unwrap().unwrap().since >= set.since);
    let log = std::fs::read_to_string(cfg.overlay.with_extension("log")).unwrap();
    assert_eq!(log.lines().count(), 3, "the unchanged repeat isn't logged");
    // The admission overlay is another file, untouched.
    assert!(!cfg.overlay.exists());

    for bad in [
        "credit 214.37 2020-01-01 200 5".to_owned(),
        format!("credit 214.373 {renews} 200 5"),
        format!("credit 214 {renews} 200"),
        format!("credit -1 {renews} 200 5"),
    ] {
        assert!(cfg.command(&words(&bad), "t").is_err(), "{bad}");
        assert!(!crate::valid_command(&words(&bad)), "{bad}");
    }
    assert!(crate::valid_command(&words(&format!(
        "credit 214.37 {renews} 200 5"
    ))));
    let off = cfg.command(&words("credit off"), "t").unwrap();
    assert!(off.contains("no room"), "{off}");
    assert_eq!(cfg.credit().unwrap(), None);
}
