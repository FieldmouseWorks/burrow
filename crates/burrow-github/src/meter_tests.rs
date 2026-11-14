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
    // Sam's installation answered with room to spare: kim's limit holds
    // kim's alone.
    assert_eq!(fresh.held("sam", t), None);
}

/// A pass bound to sam, on GH, under the meters in DIR, booked in WAKE.
fn on_sam(
    dir: &Path,
    wake: &Path,
    gh: &mut Fake,
    work: impl FnOnce(&mut dyn Api) -> Result<(), Error>,
) -> Result<Option<()>, Error> {
    pass(dir, "workers", "sam", wake, gh, work)
}

fn spent(until: u64) -> Result<crate::Response, crate::Failed> {
    ok(
        403,
        json!({"message": "API rate limit exceeded"}),
        &[
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", &until.to_string()),
        ],
    )
}

#[test]
fn a_limit_holds_the_installation_the_call_was_about_and_no_other() {
    // A REST path's repos/OWNER and orgs/OWNER, a GraphQL call's owner, and
    // for a path that names none, the installation the pass is bound to.
    type Work = fn(&mut dyn Api) -> Result<(), Error>;
    let cases: [(&str, &str, &str, Work); 4] = [
        ("kim", "GET", "repos/kim/", |api| {
            call(api, "GET", "repos/kim/r", None, None).map(|_| ())
        }),
        ("kim", "GET", "orgs/kim/", |api| {
            call(api, "GET", "orgs/kim/members", None, None).map(|_| ())
        }),
        ("kim", "POST", "graphql", |api| {
            query(api, "Kim", "query{}", json!({})).map(|_| ())
        }),
        ("sam", "GET", "search/", |api| {
            call(api, "GET", "search/issues?q=x", None, None).map(|_| ())
        }),
    ];
    for (held, method, path, work) in cases {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("meters");
        let mut gh = Fake::default();
        gh.on(method, path, vec![spent(LATER)]);
        let wake = d.path().join("wake.json");
        assert_eq!(on_sam(&dir, &wake, &mut gh, work), Ok(None), "{path}");
        assert_eq!(booked(&wake), json!({"": LATER}), "{path}");
        let mut fresh = Meters::new(Some(dir), "workers");
        let t = now();
        for owner in ["sam", "kim"] {
            assert_eq!(
                fresh.held(owner, t).is_some(),
                owner == held,
                "{path}: {owner}"
            );
        }
    }
}

#[test]
fn a_pass_asks_nothing_of_an_installation_held_already_and_goes_on_elsewhere() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("meters");
    let mut hold = crate::quota::Meter::new("workers", "kim", now());
    hold.retry_at = Some(LATER);
    hold.record_public(&dir).unwrap();
    let mut gh = Fake::default();
    gh.on("GET", "repos/sam/", vec![ok(200, json!({}), &[])]);
    let wake = d.path().join("wake.json");
    // Bound to sam, which isn't held: its own calls go, and kim's, by REST
    // path or GraphQL owner, are answered by the meter.
    let mut kims = Vec::new();
    let ran = on_sam(&dir, &wake, &mut gh, |api| {
        call(api, "GET", "repos/sam/r", None, None)?;
        kims.push(call(api, "GET", "repos/kim/r", None, None).unwrap_err());
        kims.push(query(api, "kim", "query{}", json!({})).unwrap_err());
        kims.push(call(api, "GET", "orgs/kim/members", None, None).unwrap_err());
        call(api, "GET", "repos/sam/r/issues", None, None).map(|_| ())
    });
    assert_eq!(ran, Ok(Some(())));
    assert_eq!(
        gh.asked.iter().map(|a| a.1.as_str()).collect::<Vec<_>>(),
        ["repos/sam/r", "repos/sam/r/issues"]
    );
    for e in &kims {
        assert_eq!((e.retry_at, e.unanswered), (Some(LATER), true), "{e}");
    }
    // Waiting on kim held sam for nothing.
    let mut fresh = Meters::new(Some(dir.clone()), "workers");
    assert_eq!(fresh.held("sam", now()), None);

    // And a pass whose work kim's hold stops is booked for kim's hold, with
    // nothing asked of GitHub and sam still free.
    let mut silent = silent();
    let wake2 = d.path().join("wake2.json");
    let kim = |api: &mut dyn Api| call(api, "GET", "repos/kim/r", None, None).map(|_| ());
    assert_eq!(on_sam(&dir, &wake2, &mut silent, kim), Ok(None));
    assert!(silent.asked.is_empty());
    assert_eq!(booked(&wake2), json!({"": LATER}));
    assert_eq!(fresh.held("sam", now()), None);
}

#[test]
fn an_aggregated_failure_is_booked_for_the_latest_of_its_installations_holds() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("meters");
    let mut gh = Fake::default();
    gh.on("GET", "repos/kim/", vec![spent(LATER)]);
    gh.on("GET", "repos/lee/", vec![spent(LATER + 5)]);
    let wake = d.path().join("wake.json");
    let ran = on_sam(&dir, &wake, &mut gh, |api| {
        let errors = ["kim", "lee"]
            .iter()
            .filter_map(|o| call(api, "GET", &format!("repos/{o}/r"), None, None).err())
            .collect();
        Err(Error::aggregate(errors))
    });
    assert_eq!(ran, Ok(None));
    assert_eq!(booked(&wake), json!({"": LATER + 5}));
    let mut fresh = Meters::new(Some(dir), "workers");
    let t = now();
    assert_eq!(fresh.held("kim", t).map(|h| h.until), Some(LATER));
    assert_eq!(fresh.held("lee", t).map(|h| h.until), Some(LATER + 5));
    assert_eq!(fresh.held("sam", t), None);
}

#[test]
fn a_pass_that_fails_for_another_cause_is_not_booked_nor_holds_anything() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path().join("meters");
    let mut gh = Fake::default();
    gh.on("GET", "repos/kim/", vec![spent(LATER)]);
    gh.on(
        "GET",
        "repos/sam/",
        vec![ok(404, json!({"message": "Not Found"}), &[])],
    );
    let wake = d.path().join("wake.json");
    let ran = on_sam(&dir, &wake, &mut gh, |api| {
        let errors = ["kim", "sam"]
            .iter()
            .filter_map(|o| call(api, "GET", &format!("repos/{o}/r"), None, None).err())
            .collect();
        Err(Error::aggregate(errors))
    });
    assert_eq!(ran.unwrap_err().retry_at, None);
    let mut fresh = Meters::new(Some(dir), "workers");
    assert_eq!(fresh.held("sam", now()), None);
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
