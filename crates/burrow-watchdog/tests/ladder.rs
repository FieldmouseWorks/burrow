//! A pass, step by step, with proj, burrow-events' kick port and notify
//! stood in for (common): what it does with an item past its deadline, and
//! when it looks again.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use burrow_watchdog::ladder::snooze;
use burrow_watchdog::watch::sid;
use burrow_watchdog::{ESCALATED, GRACE, IN_LINE, MIN, RENOTIFY, RETRY, SNOOZE_MAX, START};
use common::{H, Machine, T, item, named, unstick};
use serde_json::{Value, json};

// Nothing to watch: nothing done, and a pass booked only for when the
// steward, which should write every so often, would be stale. An item not
// yet due is left be, with a pass booked for its deadline.
#[test]
fn waits() {
    let m = Machine::new();
    assert_eq!(m.run(T), (Some(T + 86400.0), BTreeMap::new()));
    assert!(m.kicks().is_empty() && m.runs().is_empty());
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 10.0 * MIN, T + 30.0 * MIN, Some("autoland/sam/r"))}),
        T,
    );
    assert_eq!(m.run(T), (Some(T + 30.0 * MIN), BTreeMap::new()));
    assert!(m.kicks().is_empty());
    m.write(&m.steward, json!({}), T);
    assert_eq!(m.pass(T + MIN, false).0, Some(T + 86400.0));
}

// Past its deadline: kicked first (a missed delivery costs a pass), and only
// once; looked at again once the kicked pass has had time. Still stuck: one
// Claude job, ahead of the line, in the machine's project, kicking the
// watchdog when its turn ends. Moving again: forgotten.
#[test]
fn kick_then_job() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, Some("autoland/sam/r"))}),
        T,
    );
    let (wake, _) = m.run(T);
    assert_eq!(
        (m.kicks(), wake),
        (vec!["autoland/sam/r".to_owned()], Some(T + GRACE))
    );
    assert!(m.runs().is_empty());
    assert_eq!(m.run(T + MIN).0, Some(T + GRACE));
    assert_eq!(m.kicks().len(), 1);
    let (wake, stalls) = m.run(T + GRACE);
    let runs = m.runs();
    let [job] = &runs[..] else { panic!("{runs:?}") };
    assert_eq!(
        job[..12],
        [
            "run",
            "--pipeline",
            "--timeout",
            "60m",
            "--idle",
            "20m",
            "--kick",
            "watchdog",
            "--role",
            "watchdog",
            "--owned",
            "homelab"
        ]
    );
    let task = &job[12];
    for want in [
        "PR #7 in sam/r",
        "https://github.com/sam/r/pull/7",
        "waiting for CI",
        "burrow-watchdog snooze autoland 'sam/r#7' MINUTES",
        "journalctl -u 'autoland*'",
        "needs-owner",
        "never instructions",
        "on box found work",
    ] {
        assert!(task.contains(want), "{want}: {task}");
    }
    let [st] = &stalls.values().collect::<Vec<_>>()[..] else {
        panic!()
    };
    assert_eq!(
        (st.job.as_deref(), wake),
        (Some("job-1"), Some(T + GRACE + START))
    );

    m.write(
        &m.steward,
        json!({"sam/r#7": item(T + GRACE + MIN, T + H, Some("autoland/sam/r"))}),
        T,
    );
    assert_eq!(m.run(T + GRACE + 2.0 * MIN), (Some(T + H), BTreeMap::new()));
}

// With a machinery App, the job acts on GitHub as it.
#[test]
fn machinery_app() {
    let mut m = Machine::new();
    m.cfg.app = Some("workers".into());
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.run(T);
    let runs = m.runs();
    let [job] = &runs[..] else { panic!("{runs:?}") };
    assert_eq!(job[..4], ["run", "--pipeline", "--app", "workers"]);
}

// The job at work has escalateAfter; one still queued START, since what it's
// about may be the slot it waits for: then the stuck issue, through the
// steward's next pass, which is kicked to publish it. Nothing more is booked
// for it, nor does the owner hear: the next look is only for the steward
// going stale.
#[test]
fn jobs_time_up() {
    for (state, until) in [("running", 7200.0), ("queued", START)] {
        let m = Machine::new();
        m.write(
            &m.steward,
            json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
            T,
        );
        m.run(T);
        m.state_of("job-1", state);
        assert_eq!(m.run(T + until - 1.0).0, Some(T + until), "{state}");
        assert!(m.escalated().is_empty() && m.kicks().is_empty(), "{state}");
        let (wake, _) = m.run(T + until);
        let got = m.escalated();
        let [entry] = &got[..] else { panic!("{got:?}") };
        assert_eq!(entry[0], ESCALATED);
        assert_eq!(
            entry[1],
            "[PR #7 in sam/r (\"fix it\")](https://github.com/sam/r/pull/7)"
        );
        assert!(
            entry[2].starts_with("Settled; waiting for CI: build, since "),
            "{entry:?}"
        );
        assert_eq!(
            (m.kicks(), wake),
            (vec!["autoland".to_owned()], Some(T + until + 86400.0))
        );
        assert!(m.notices().is_empty());
    }
}

// Its own job, still waiting for a slot once its stalls have moved on, is
// withdrawn (proj withdraw): it has done nothing, and would only take a slot
// from the line for what's no longer its. Moving again, or on to the stuck
// issue (at START, say): at once. One proj can't settle is asked about again
// on the next pass. A state from before it kept these has the jobs of its
// stalls on the issue to go.
#[test]
fn withdraws_its_job() {
    let stalled = |m: &Machine| {
        m.write(
            &m.steward,
            json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
            T,
        );
        m.run(T);
        m.state_of("job-1", "queued");
        m.run(T + MIN);
        assert!(m.withdrawals().is_empty());
    };
    let gone = |m: &Machine| m.world.states.borrow()["job-1"] == "failed";
    let kept = |m: &Machine| m.state().withdraw.unwrap_or_default();

    let m = Machine::new();
    stalled(&m);
    m.write(&m.steward, json!({}), T);
    assert_eq!(m.run(T + 2.0 * MIN).1, BTreeMap::new());
    assert!(m.withdrawals() == ["job-1"] && gone(&m));
    m.run(T + 3.0 * MIN);
    assert_eq!(m.withdrawals(), ["job-1"]);
    assert!(kept(&m).is_empty());

    let m = Machine::new();
    stalled(&m);
    m.world.stuck.set(true);
    m.run(T + START);
    assert_eq!(
        (m.escalated().len(), m.withdrawals()),
        (1, vec!["job-1".into()])
    );
    assert_eq!(kept(&m), BTreeSet::from(["job-1".into()]));
    m.world.stuck.set(false);
    m.run(T + START + MIN);
    assert!(m.withdrawals() == ["job-1", "job-1"] && gone(&m));
    m.run(T + START + 2.0 * MIN);
    assert!(m.withdrawals().len() == 2 && kept(&m).is_empty());
    assert_eq!((m.escalated().len(), m.notices()), (1, vec![]));

    let m = Machine::new();
    stalled(&m);
    m.world.stuck.set(true);
    m.run(T + START);
    let path = m.cfg.state_dir.join("state.json");
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved.as_object_mut().unwrap().remove("withdraw");
    fs::write(&path, saved.to_string()).unwrap();
    m.world.stuck.set(false);
    m.run(T + START + MIN);
    assert!(m.withdrawals() == ["job-1", "job-1"] && gone(&m));
}

// Its own job is left be once it's at work, and while a stall it was handed
// with still has it: one job takes every stall that's ready in a pass, and
// one of them moving on leaves the job the rest. One the owner hears of
// keeps its job too (here, with no steward), queued or not: it's the only
// Claude still on it.
#[test]
fn keeps_a_job_it_needs() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.run(T);
    m.state_of("job-1", "running");
    m.write(&m.steward, json!({}), T);
    m.run(T + MIN);
    m.run(T + 2.0 * MIN);
    assert_eq!(m.withdrawals(), ["job-1"]);
    assert_eq!(m.world.states.borrow()["job-1"], "running");
    assert_eq!(m.state().withdraw, Some(BTreeSet::new()));

    let m = Machine::new();
    let mut q = item(T - 3.0 * H, T - MIN, None);
    q["what"] = "PR #2 in sam/q".into();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None), "sam/q#2": q}),
        T,
    );
    m.run(T);
    assert_eq!(m.runs().len(), 1);
    m.state_of("job-1", "queued");
    m.write(&m.steward, json!({"sam/q#2": q}), T);
    assert_eq!(m.run(T + MIN).1.len(), 1);
    assert!(m.withdrawals().is_empty());
    m.write(&m.steward, json!({}), T);
    m.run(T + 2.0 * MIN);
    assert_eq!(m.withdrawals(), ["job-1"]);

    let mut m = Machine::new();
    m.cfg.steward_kick = None;
    m.write(
        &m.line,
        json!({"homelab-1": named("proj job homelab-1", "waiting for a slot", T - H, T - MIN)}),
        T,
    );
    m.run(T);
    m.state_of("job-1", "queued");
    m.run(T + START);
    m.run(T + START + MIN);
    assert_eq!((m.notices().len(), m.withdrawals()), (1, vec![]));
}

// A job that ended with it still stuck: its fix gets GRACE to show, then the
// stuck issue, and that's the last step: the issue's own job has it, however
// long it lasts, and the owner never hears of it. Once it moves, it's gone
// from the issue.
#[test]
fn stuck_issue_is_last() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.run(T);
    m.state_of("job-1", "done");
    assert_eq!(m.run(T + MIN).0, Some(T + MIN + GRACE));
    assert!(m.escalated().is_empty());
    m.run(T + MIN + GRACE);
    assert_eq!(
        (m.escalated().len(), m.kicks()),
        (1, vec!["autoland".to_owned()])
    );
    for later in [4.0 * H, 3.0 * 86400.0] {
        m.run(T + MIN + GRACE + later);
    }
    assert_eq!((m.notices(), m.escalated().len()), (vec![], 1));

    m.write(&m.steward, json!({}), T);
    assert_eq!(m.run(T + 4.0 * 86400.0).1, BTreeMap::new());
    assert_eq!((m.notices(), m.escalated().len()), (vec![], 0));
    assert_eq!(m.kicks().last().map(String::as_str), Some("autoland"));
}

// The owner was told of a stall the stuck issue has now (told before its own
// job took the issue on): that notice is closed, saying so, and they aren't
// told again. A close that doesn't get through is sent again every pass.
#[test]
fn old_notice_closes() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.run(T);
    m.run(T + MIN);
    m.run(T + MIN + GRACE);
    assert_eq!(m.escalated().len(), 1);
    let s = sid("autoland", "sam/r#7", T - 3.0 * H);
    let key = burrow_watchdog::ladder::notice_key(&s);
    let path = m.cfg.state_dir.join("state.json");
    let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    saved["stalls"][&s]["told"] = (T + MIN + GRACE).into();
    fs::write(&path, saved.to_string()).unwrap();
    m.world.kept.borrow_mut().insert(key.clone());
    m.world.opened.borrow_mut().insert(key.clone());

    m.world.down.set(true);
    m.run(T + H);
    m.run(T + H + MIN);
    m.world.down.set(false);
    let (_, stalls) = m.run(T + H + 2.0 * MIN);
    let on_issue = [
        "close",
        key.as_str(),
        "It's on the pipeline's stuck issue now, whose own Claude job takes it from there.",
    ];
    assert_eq!(m.notices(), vec![on_issue; 3]);
    assert!(stalls[&s].told.is_none() && stalls[&s].entry.is_some());
    assert_eq!((m.opened(), m.escalated().len()), (0, 1));
    m.run(T + H + RENOTIFY);
    assert_eq!(m.notices().len(), 3);
}

// The stuck issue's own job is the one stall it can't take, since it would
// only hand it to itself: waiting for its slot past its deadline, the owner
// hears at once (a job of its own would only queue too); at work past it, a
// job looks first. Told, the notice is sent again every RENOTIFY while it
// lasts (notify can't say it got through), by whatever pass comes first.
// Once it moves, the notice closes and nothing's left of the stall.
#[test]
fn unstick_is_the_owners() {
    let m = Machine::new();
    let told = m.told_at();
    assert!(m.runs().is_empty() && m.escalated().is_empty());
    let notices = m.notices();
    let [notice] = &notices[..] else {
        panic!("{notices:?}")
    };
    assert_eq!(notice[0], "open");
    assert!(
        notice[1].starts_with("stalled-") && notice[1].len() == 20,
        "{notice:?}"
    );
    assert_eq!(notice[2], "Stuck: proj job homelab-9");
    for want in [
        "waiting for the --unstick slot",
        "takes the pipeline's stuck issue",
        "on box show",
    ] {
        assert!(notice[3].contains(want), "{want}: {notice:?}");
    }
    assert_eq!(m.opened(), 1);
    m.run(told + RENOTIFY - 1.0);
    assert_eq!(m.notices().len(), 1);
    m.run(told + RENOTIFY);
    assert_eq!((&m.notices()[1], m.opened()), (notice, 1));

    m.write(&m.line, json!({}), T);
    assert_eq!(m.run(told + 2.0 * RENOTIFY).1, BTreeMap::new());
    assert_eq!(m.notices()[2], ["close", &notice[1], "It's moving again."]);
    assert_eq!((m.opened(), m.escalated().len()), (0, 0));
    assert!(m.state().closing.is_empty());

    let m = Machine::new();
    m.write(&m.line, json!({"homelab-9": unstick(false)}), T);
    m.run(T);
    assert_eq!(m.runs().len(), 1);
    m.state_of("job-1", "done");
    m.run(T + MIN);
    m.run(T + MIN + GRACE);
    let notices = m.notices();
    let [notice] = &notices[..] else {
        panic!("{notices:?}")
    };
    assert!(
        notice[0] == "open" && notice[3].contains("takes the pipeline's stuck issue"),
        "{notice:?}"
    );
    assert!(m.escalated().is_empty());
}

// GitHub down: a notice that didn't get through is sent again once it's
// back, and a close is sent on every pass until it has, days if need be.
#[test]
fn github_down() {
    let m = Machine::new();
    m.world.down.set(true);
    let told = m.told_at();
    assert_eq!((m.notices()[0][0].as_str(), m.opened()), ("open", 0));
    m.world.down.set(false);
    m.run(told + RENOTIFY);
    assert_eq!(m.opened(), 1);
    m.world.down.set(true);
    m.write(&m.line, json!({}), T);
    for day in 1..=4 {
        m.run(T + 86400.0 * day as f64);
        assert_eq!((m.opened(), m.state().closing.len()), (1, 1), "day {day}");
    }
    m.world.down.set(false);
    m.run(T + 86400.0 * 5.0);
    assert_eq!((m.opened(), m.state().closing.len()), (0, 0));
    let closes = m.notices().len();
    m.run(T + 86400.0 * 6.0);
    assert_eq!(m.notices().len(), closes);
}

// Back as it was before its close got through (gone from the file a while,
// its source stale, its deadline moved): it carries on from where it had got
// to, and its notice, still open, isn't closed under it.
#[test]
fn back_again() {
    let m = Machine::new();
    m.told_at();
    assert_eq!(m.opened(), 1);
    m.world.down.set(true);
    m.write(&m.line, json!({}), T);
    m.run(T + 6.0 * H);
    m.world.down.set(false);
    let closes = || m.notices().iter().filter(|n| n[0] == "close").count();
    assert_eq!(closes(), 1);
    m.write(&m.line, json!({"homelab-9": unstick(true)}), T);
    let (_, stalls) = m.run(T + 6.0 * H + MIN);
    let [st] = &stalls.values().collect::<Vec<_>>()[..] else {
        panic!()
    };
    assert!(st.told.is_some() && m.state().closing.is_empty());
    m.run(T + 6.0 * H + 2.0 * MIN);
    assert_eq!((m.opened(), closes()), (1, 1));
}

// Every stall ready in one pass goes to one job: often one cause. The
// watchdog's own job waiting in proj's line is its own steps' to time.
#[test]
fn one_job_a_pass() {
    let m = Machine::new();
    let mut q = item(T - 3.0 * H, T - MIN, None);
    q["what"] = "PR #2 in sam/q".into();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None), "sam/q#2": q}),
        T,
    );
    m.write(
        &m.line,
        json!({"homelab-1": named("proj job homelab-1", "waiting for a slot", T - H, T - MIN)}),
        T,
    );
    m.run(T);
    let runs = m.runs();
    let [job] = &runs[..] else { panic!("{runs:?}") };
    for want in ["PR #7", "PR #2", "proj job homelab-1"] {
        assert!(job[12].contains(want), "{want}");
    }
    m.write(
        &m.line,
        json!({"job-1": named("proj job job-1", "waiting for a slot", T - H, T - MIN)}),
        T,
    );
    assert_eq!(
        m.run(T + MIN).1.into_keys().collect::<BTreeSet<_>>(),
        BTreeSet::from([
            sid("autoland", "sam/r#7", T - 3.0 * H),
            sid("autoland", "sam/q#2", T - 3.0 * H)
        ])
    );
}

// A turn that's only waiting for a slot gets no job of its own, which would
// join the line ahead of it: it goes straight to the stuck issue, whose one
// job looks at the line as a whole (it never waits in it), and stays there:
// the owner doesn't hear.
#[test]
fn in_line() {
    let m = Machine::new();
    let waits = |key: &str| {
        let mut it = named(
            &format!("proj job {key}"),
            "turn 1 is waiting in line for a slot",
            T - 3.0 * H,
            T - MIN,
        );
        it["line"] = true.into();
        it
    };
    m.write(
        &m.line,
        json!({"homelab-1": waits("homelab-1"), "homelab-2": waits("homelab-2")}),
        T,
    );
    m.run(T);
    assert_eq!(m.runs(), Vec::<Vec<String>>::new());
    let got = m.escalated();
    assert!(
        got.len() == 2 && got.iter().all(|e| e[0] == IN_LINE),
        "{got:?}"
    );
    assert!(m.kicks().contains(&"autoland".to_owned()));
    // Anything else that's stuck beside it still gets its job, and the job's
    // task doesn't name what's in line.
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.run(T + MIN);
    let runs = m.runs();
    let [job] = &runs[..] else { panic!("{runs:?}") };
    assert!(job[12].contains("PR #7"));
    assert!(!job[12].contains("homelab-1"));
    m.run(T + 86400.0);
    assert!(m.notices().is_empty());
}

// A stall already on the stuck issue that the issue can't take now goes on
// to the owner, and off the issue: when there's no steward any more to
// publish it, or when it turns out to be that issue's own job (made one
// later, proj unstick), which the issue would only hand to itself. One the
// owner was told of before (as main told them after ownerAfter, keeping it
// on the issue) comes off too, its notice kept open and sent again.
#[test]
fn issue_cant_take_it() {
    for (gone, told) in [(true, false), (false, false), (true, true), (false, true)] {
        let why = format!("gone {gone}, told {told}");
        let mut m = Machine::new();
        m.write(&m.steward, json!({}), T);
        let mut it = named(
            "proj job homelab-9",
            "turn 1 is waiting in line for a slot",
            T - 3.0 * H,
            T - MIN,
        );
        it["line"] = true.into();
        m.write(&m.line, json!({"homelab-9": it.clone()}), T);
        m.run(T);
        assert_eq!((m.escalated().len(), m.notices().len()), (1, 0), "{why}");
        let s = sid("proj", "homelab-9", T - 3.0 * H);
        let key = burrow_watchdog::ladder::notice_key(&s);
        if told {
            let path = m.cfg.state_dir.join("state.json");
            let mut saved: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            saved["stalls"][&s]["told"] = T.into();
            fs::write(&path, saved.to_string()).unwrap();
            m.world.kept.borrow_mut().insert(key.clone());
            m.world.opened.borrow_mut().insert(key.clone());
        }
        if gone {
            m.cfg.steward_kick = None;
        } else {
            it["unstick"] = true.into();
            m.write(&m.line, json!({"homelab-9": it}), T);
        }
        m.run(T + MIN);
        assert!(m.escalated().is_empty(), "{why}");
        let opens = || m.notices().iter().filter(|n| n[0] == "open").count();
        assert!(
            m.notices().iter().all(|n| n[0] == "open"),
            "{why}: {:?}",
            m.notices()
        );
        assert_eq!((opens(), m.opened()), (usize::from(!told), 1), "{why}");
        m.run(T + MIN + RENOTIFY);
        assert_eq!(opens(), 1 + usize::from(!told), "{why}");
    }
}

// A job that couldn't start is tried again RETRY later, and after
// escalateAfter it's the next step's.
#[test]
fn no_job() {
    let m = Machine::new();
    m.world.refuse.set(true);
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    assert_eq!(m.run(T).0, Some(T + RETRY));
    assert_eq!(m.runs().len(), 1);
    m.run(T + RETRY - 1.0);
    assert_eq!(m.runs().len(), 1);
    m.run(T + RETRY);
    assert_eq!((m.runs().len(), m.escalated().len()), (2, 0));
    m.run(T + 7200.0);
    assert_eq!(m.escalated().len(), 1);
}

// Each look comes by the time its next step could, even with an
// escalateAfter shorter than START or RETRY: a queued job's, or one that
// couldn't start. And a retry's deadline between two retries, booked by a
// pass for something else in between all the same.
#[test]
fn looks_in_time() {
    for refuse in [false, true] {
        let mut m = Machine::new();
        m.cfg.escalate_after = 60.0;
        m.world.refuse.set(refuse);
        m.write(
            &m.steward,
            json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
            T,
        );
        assert_eq!(m.run(T).0, Some(T + 60.0), "{refuse}");
        m.state_of("job-1", "queued");
        m.run(T + 60.0);
        assert_eq!(m.escalated().len(), 1, "{refuse}");
    }
    let mut m = Machine::new();
    m.cfg.escalate_after = RETRY + 60.0;
    m.world.refuse.set(true);
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.run(T);
    assert_eq!(m.run(T + RETRY).0, Some(T + RETRY + 60.0));
    assert_eq!(m.runs().len(), 2);
    assert_eq!(m.run(T + RETRY + 30.0).0, Some(T + RETRY + 60.0));
    assert_eq!(m.runs().len(), 2);
    m.run(T + RETRY + 60.0);
    assert_eq!(m.escalated().len(), 1);
}

// A stale source is one stall, not one an item: its kick, then a job. When
// that's the steward, the owner hears in place of its stuck issue, which it
// couldn't publish. A source that has never written its file goes stale
// from when that was first seen.
#[test]
fn stale_source() {
    let mut m = Machine::new();
    m.cfg.sources.get_mut("autoland").unwrap().stale = Some(7200.0);
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, Some("autoland/sam/r")), "sam/q#2": item(T - 3.0 * H, T - MIN, None)}),
        T - 7200.0,
    );
    let (_, stalls) = m.pass(T, false);
    let [st] = &stalls.values().collect::<Vec<_>>()[..] else {
        panic!("{stalls:?}")
    };
    assert_eq!(
        (st.key.as_str(), st.item.what.as_str()),
        ("", "the PR steward")
    );
    assert_eq!(m.kicks(), ["autoland"]);
    assert!(
        st.item.phase.contains("hasn't changed since"),
        "{}",
        st.item.phase
    );
    m.pass(T + GRACE, false);
    let runs = m.runs();
    let [job] = &runs[..] else { panic!("{runs:?}") };
    assert!(
        job[12].contains("the PR steward")
            && job[12].contains("burrow-watchdog snooze autoland - MINUTES")
    );
    m.state_of("job-1", "running");
    m.pass(T + GRACE + 7200.0, false);
    assert!(m.escalated().is_empty());
    assert_eq!(m.notices()[0][2], "Stuck: the PR steward");

    let mut m = Machine::new();
    m.cfg.sources.get_mut("autoland").unwrap().stale = Some(7200.0);
    assert_eq!(m.run(T).0, Some(T + 7200.0));
    let (_, stalls) = m.run(T + 7200.0);
    let phase = &stalls.values().next().unwrap().item.phase;
    assert!(
        phase.starts_with("it has written no watch file since"),
        "{phase}"
    );
}

// Without a steward, the step after the job is the owner.
#[test]
fn no_steward() {
    let mut m = Machine::new();
    m.cfg.steward_kick = None;
    m.write(
        &m.line,
        json!({"homelab-1": named("proj job homelab-1", "waiting for a slot", T - H, T - MIN)}),
        T,
    );
    m.run(T);
    m.state_of("job-1", "done");
    m.run(T + MIN);
    m.run(T + MIN + GRACE);
    assert_eq!((m.notices().len(), m.escalated().len()), (1, 0));
    assert!(m.kicks().is_empty());
}

// A job that finds it legitimately under way gives it more time, once, up
// to SNOOZE_MAX; meanwhile no step is taken. Its time up and still stuck:
// the next step, as its job has ended.
#[test]
fn snoozes() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"sam/r#7": item(T - 3.0 * H, T - MIN, None)}),
        T,
    );
    m.run(T);
    snooze(
        &m.cfg,
        "autoland",
        "sam/r#7",
        999,
        "its gate's queue is two hours deep",
        T + MIN,
    )
    .unwrap();
    m.state_of("job-1", "done");
    let until = T + MIN + SNOOZE_MAX as f64 * MIN;
    assert_eq!(m.run(T + 2.0 * MIN).0, Some(until));
    assert!(m.escalated().is_empty());
    let again = snooze(&m.cfg, "autoland", "sam/r#7", 30, "again", T + 3.0 * MIN).unwrap_err();
    assert!(again.contains("once already"), "{again}");
    let none = snooze(&m.cfg, "autoland", "sam/x#1", 30, "nothing", T).unwrap_err();
    assert!(none.contains("nothing to snooze"), "{none}");
    m.run(until);
    assert_eq!(m.escalated().len(), 1);
}

// What a source writes that isn't an item is left out, not a crash, and half
// a file is no file.
#[test]
fn bad_files() {
    let m = Machine::new();
    m.write(
        &m.steward,
        json!({"a": "no", "b": {"what": "x", "phase": "y", "since": "1", "due": T - 1.0},
               "": item(T - 9.0, T - 1.0, None), "c": item(T - 9.0, T - 1.0, None)}),
        T,
    );
    assert_eq!(
        m.run(T).1.into_keys().collect::<Vec<_>>(),
        [sid("autoland", "c", T - 9.0)]
    );
    fs::write(&m.steward, "{half").unwrap();
    assert_eq!(m.pass(T, false).1, BTreeMap::new());
}
