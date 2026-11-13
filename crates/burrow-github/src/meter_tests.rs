//! Every caller on one installation waits on its one meter: what one met,
//! the next is held by before it asks GitHub anything, and both book the
//! same time. Callers here are the scripted GitHub (fake.rs) on the clock.

use crate::deferred::run_kicking;
use crate::fake::{Fake, ok};
use crate::quota::{Hold, Meters};
use crate::{Api, Error, call, query};
use serde_json::{Value, json};
use std::{fs, path::Path};

/// Far enough off that it's still to come whenever the test runs.
const LATER: u64 = 4_000_000_000;

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn ask(api: &mut dyn Api) -> Result<(), Error> {
    call(api, "GET", "repos/sam/r/issues", None, None).map(|_| ())
}

fn booked(wake: &Path) -> Value {
    serde_json::from_slice(&fs::read(wake).unwrap()).unwrap()
}

/// A caller whose GitHub has nothing scripted: any call it made would panic.
fn silent() -> Fake {
    Fake::default()
}

/// One caller's pass on sam's installation, as the App `app`, under the
/// meters in `dir`, booked in `wake`.
fn pass(
    dir: &Path,
    app: &str,
    owner: &str,
    wake: &Path,
    api: &mut dyn Api,
    work: impl FnOnce(&mut dyn Api) -> Result<(), Error>,
) -> Result<Option<()>, Error> {
    let mut meters = Meters::new(Some(dir.to_owned()), app);
    run_kicking(
        Some(wake),
        None,
        api,
        now(),
        Some((&mut meters, owner)),
        work,
    )
}

#[test]
fn a_second_caller_on_the_installation_asks_nothing_and_books_the_same_time() {
    let d = tempfile::tempdir().unwrap();
    let meters = d.path().join("meters");
    let mut first = Fake::default();
    first.on(
        "GET",
        "repos/sam/",
        vec![ok(
            403,
            json!({"message": "API rate limit exceeded"}),
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &LATER.to_string()),
            ],
        )],
    );
    let wake = d.path().join("first.json");
    assert_eq!(
        pass(&meters, "workers", "sam", &wake, &mut first, ask),
        Ok(None)
    );
    assert_eq!(booked(&wake), json!({"": LATER}));
    assert_eq!(first.asked.len(), 1);

    // Another program, started afterwards, on the same installation.
    let mut second = silent();
    let wake2 = d.path().join("second.json");
    assert_eq!(
        pass(&meters, "workers", "Sam", &wake2, &mut second, ask),
        Ok(None)
    );
    assert!(second.asked.is_empty(), "{:?}", second.asked);
    assert_eq!(booked(&wake2), json!({"": LATER}));
}

#[test]
fn a_secondary_limit_holds_the_installation_not_only_its_caller() {
    let d = tempfile::tempdir().unwrap();
    let meters = d.path().join("meters");
    let mut first = Fake::default();
    // No budget spent: only retry-after says it's a limit, and for how long.
    first.on(
        "GET",
        "repos/sam/",
        vec![ok(
            403,
            json!({"message": "You have exceeded a secondary rate limit"}),
            &[("retry-after", "600"), ("x-ratelimit-remaining", "4000")],
        )],
    );
    let wake = d.path().join("first.json");
    assert_eq!(
        pass(&meters, "workers", "sam", &wake, &mut first, ask),
        Ok(None)
    );
    let at = booked(&wake)[""].as_u64().unwrap();
    assert!(at >= now() + 590, "{at}");

    let mut second = silent();
    let wake2 = d.path().join("second.json");
    assert_eq!(
        pass(&meters, "workers", "sam", &wake2, &mut second, ask),
        Ok(None)
    );
    assert!(second.asked.is_empty());
    assert_eq!(booked(&wake2), json!({"": at}));
}

#[test]
fn a_graphql_limit_holds_the_installation_not_only_its_caller() {
    let d = tempfile::tempdir().unwrap();
    let meters = d.path().join("meters");
    let mut first = Fake::default();
    first.on(
        "POST",
        "graphql",
        vec![ok(
            200,
            json!({"errors": [{"type": "RATE_LIMITED", "message": "budget spent"}]}),
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &LATER.to_string()),
            ],
        )],
    );
    let wake = d.path().join("first.json");
    let graphql = |api: &mut dyn Api| query(api, "sam", "query{}", json!({})).map(|_| ());
    assert_eq!(
        pass(&meters, "workers", "sam", &wake, &mut first, graphql),
        Ok(None)
    );
    assert_eq!(booked(&wake), json!({"": LATER}));

    // A REST pass on the same installation: GraphQL's limit holds it too.
    let mut second = silent();
    let wake2 = d.path().join("second.json");
    assert_eq!(
        pass(&meters, "workers", "sam", &wake2, &mut second, ask),
        Ok(None)
    );
    assert!(second.asked.is_empty());
    assert_eq!(booked(&wake2), json!({"": LATER}));
}

#[test]
fn one_installations_hold_never_holds_another_app_or_owner() {
    let d = tempfile::tempdir().unwrap();
    let meters = d.path().join("meters");
    let mut first = Fake::default();
    first.on(
        "GET",
        "repos/sam/",
        vec![ok(
            429,
            json!({"message": "slow down"}),
            &[("retry-after", "600")],
        )],
    );
    let wake = d.path().join("first.json");
    assert_eq!(
        pass(&meters, "workers", "sam", &wake, &mut first, ask),
        Ok(None)
    );

    for (app, owner) in [("other", "sam"), ("workers", "kim")] {
        let mut gh = Fake::default();
        gh.on("GET", "repos/", vec![ok(200, json!({}), &[])]);
        let wake = d.path().join(format!("{app}-{owner}.json"));
        let work = |api: &mut dyn Api| {
            call(api, "GET", &format!("repos/{owner}/r/issues"), None, None).map(|_| ())
        };
        assert_eq!(
            pass(&meters, app, owner, &wake, &mut gh, work),
            Ok(Some(()))
        );
        assert_eq!(gh.asked.len(), 1, "{app} {owner}");
        assert_eq!(booked(&wake), json!({}));
    }
}

#[test]
fn every_answer_feeds_the_installation_its_path_names() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("meters");
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/kim/",
        vec![ok(
            403,
            json!({}),
            &[
                ("x-ratelimit-remaining", "0"),
                ("x-ratelimit-reset", &LATER.to_string()),
            ],
        )],
    );
    gh.on(
        "GET",
        "repos/sam/",
        vec![ok(200, json!({}), &[("x-ratelimit-remaining", "4321")])],
    );
    let mut meters = Meters::new(Some(dir.clone()), "workers");
    let t = now();
    // A pass bound to sam that also reads one of kim's repos: kim's answer
    // is kim's installation's, whose token it was asked with.
    let result = crate::deferred::metered(Some((&mut meters, "sam")), &mut gh, t, |api| {
        call(api, "GET", "repos/sam/r", None, None)?;
        call(api, "GET", "repos/kim/r", None, None).map(|_| ())
    });
    assert!(result.is_err());
    let mut fresh = Meters::new(Some(dir), "workers");
    assert_eq!(
        fresh.held("kim", t),
        Some(Hold {
            app: "workers".into(),
            owner: "kim".into(),
            until: LATER
        })
    );
    // The error met on kim is booked against the pass's own installation
    // too (Meters::met): the pass can't go on there either.
    assert!(fresh.held("sam", t).is_some());
}

#[test]
fn a_caller_that_cant_record_still_books_and_holds_itself() {
    let d = tempfile::tempdir().unwrap();
    let blocked = d.path().join("blocked");
    fs::write(&blocked, "not a dir").unwrap();
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/",
        vec![ok(429, json!({}), &[("retry-after", "600")])],
    );
    let mut meters = Meters::new(Some(blocked), "workers");
    let wake = d.path().join("wake.json");
    let t = now();
    let ran = run_kicking(
        Some(&wake),
        None,
        &mut gh,
        t,
        Some((&mut meters, "sam")),
        ask,
    );
    assert_eq!(ran, Ok(None));
    let at = booked(&wake)[""].as_u64().unwrap();
    assert_eq!(meters.held("sam", t).map(|h| h.until), Some(at));
    assert_eq!(meters.holds(t).len(), 1);
}

#[test]
fn without_a_wake_file_a_held_installation_is_its_limit_error() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("meters");
    let mut hold = crate::quota::Meter::new("workers", "sam", now());
    hold.retry_at = Some(LATER);
    hold.record_public(&dir).unwrap();
    let mut meters = Meters::new(Some(dir), "workers");
    let mut gh = silent();
    let e = run_kicking(None, None, &mut gh, now(), Some((&mut meters, "sam")), ask).unwrap_err();
    assert_eq!((e.retry_at, e.unanswered), (Some(LATER), true));
    assert!(gh.asked.is_empty());
}

#[test]
fn a_spent_budget_after_another_programs_run_is_the_installations_hold() {
    for (remaining, held) in [(0, true), (12, false)] {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("meters");
        let mut gh = Fake::default();
        gh.on(
            "GET",
            "rate_limit",
            vec![ok(
                200,
                json!({"resources": {"core": {"remaining": remaining, "reset": LATER}}}),
                &[],
            )],
        );
        let mut meters = Meters::new(Some(dir.clone()), "workers");
        let got = meters.spent(&mut gh, "sam");
        assert_eq!(got.is_some(), held);
        if held {
            assert_eq!(got.unwrap().until, LATER);
            // And the next caller on the installation finds it.
            let mut fresh = Meters::new(Some(dir), "workers");
            assert_eq!(fresh.held("sam", now()).map(|h| h.until), Some(LATER));
        }
    }
}

#[test]
fn door_meter_is_empty_or_every_field() {
    use crate::deferred::DoorMeter;
    assert_eq!(DoorMeter::parse(""), Ok(None));
    assert_eq!(
        DoorMeter::parse(r#"{"dir":"/q","app":"workers","owner":"Sam"}"#),
        Ok(Some(DoorMeter {
            dir: "/q".into(),
            app: "workers".into(),
            owner: "Sam".into()
        }))
    );
    assert!(DoorMeter::parse(r#"{"dir":"/q","app":"workers"}"#).is_err());
}
