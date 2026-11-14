//! A spent budget is a hold until its reset, whichever reading the meter
//! keeps the budget of: no sample of the same second, older sample
//! arriving late, healthy reading of another resource (REST's, GraphQL's)
//! or budget probe lifts it, in a process's meter or the shared record.

use crate::Response;
use crate::quota::{Meter, Meters};
use serde_json::json;

const RESET: u64 = 5000;

fn answer(remaining: u64, reset: u64) -> Response {
    Response {
        status: 200,
        headers: vec![
            ("x-ratelimit-remaining".into(), remaining.to_string()),
            ("x-ratelimit-reset".into(), reset.to_string()),
        ],
        body: "{}".into(),
    }
}

fn reading(at: u64, remaining: u64, reset: u64) -> Meter {
    Meter::reading("workers", "sam", &answer(remaining, reset), at)
}

#[test]
fn the_last_point_spent_in_the_same_second_holds() {
    let mut meter = Meter::new("workers", "sam", 0);
    meter.observe(&answer(1, RESET), 1000);
    meter.observe(&answer(0, RESET), 1000);
    assert_eq!(meter.until(1000), Some(RESET));
}

#[test]
fn a_newer_reading_with_room_on_another_resource_doesnt_lift_a_spent_one() {
    let mut meter = Meter::new("workers", "sam", 0);
    // GraphQL's budget spent, by a successful answer; then a REST answer
    // with room, a second later.
    meter.observe(&answer(0, RESET), 1000);
    meter.observe(&answer(4000, RESET + 100), 1001);
    assert_eq!((meter.at, meter.remaining), (1001, Some(4000)));
    assert_eq!(meter.until(1001), Some(RESET));
}

#[test]
fn a_late_older_spent_reading_still_holds() {
    let mut meter = reading(1100, 4000, RESET + 100);
    meter.merge(reading(1000, 0, RESET));
    assert_eq!((meter.at, meter.remaining), (1100, Some(4000)));
    assert_eq!(meter.until(1100), Some(RESET));
}

#[test]
fn the_shared_record_keeps_a_spent_budget_whatever_order_readings_come_in() {
    for (first, second) in [
        // In order, the same second, and late.
        (reading(1000, 0, RESET), reading(1001, 4000, RESET + 100)),
        (reading(1000, 1, RESET), reading(1000, 0, RESET)),
        (reading(1000, 0, RESET), reading(1000, 1, RESET)),
        (reading(1100, 4000, RESET + 100), reading(1000, 0, RESET)),
    ] {
        let d = tempfile::tempdir().unwrap();
        first.record_public(d.path()).unwrap();
        second.record_public(d.path()).unwrap();
        let mut meters = Meters::new(Some(d.path().to_owned()), "workers");
        assert_eq!(
            meters.held("sam", 1100).map(|h| h.until),
            Some(RESET),
            "{first:?} then {second:?}"
        );
    }
}

#[test]
fn a_process_records_the_last_point_it_spent_for_the_next_caller() {
    let d = tempfile::tempdir().unwrap();
    let mut meters = Meters::new(Some(d.path().to_owned()), "workers");
    meters.observe("sam", &answer(1, RESET), 1000);
    meters.observe("sam", &answer(0, RESET), 1000);
    let mut next = Meters::new(Some(d.path().to_owned()), "workers");
    assert_eq!(next.held("sam", 1000).map(|h| h.until), Some(RESET));
}

#[test]
fn a_budget_probe_keeps_either_resources_spent_budget() {
    // Spent before the probe, which finds REST's with room.
    let mut meter = reading(1000, 0, RESET);
    meter.at = 1001;
    meter.budget(&json!({"resources": {
        "core": {"remaining": 4000, "reset": RESET + 100},
        "graphql": {"remaining": 10, "reset": RESET + 200},
    }}));
    assert_eq!(meter.remaining, Some(4000));
    assert_eq!(meter.until(1001), Some(RESET));
    // GraphQL's spent, REST's with room: held until GraphQL's comes back.
    let mut meter = Meter::new("workers", "sam", 1000);
    meter.budget(&json!({"resources": {
        "core": {"remaining": 4000, "reset": RESET + 100},
        "graphql": {"remaining": 0, "reset": RESET + 200},
    }}));
    assert_eq!(meter.until(1000), Some(RESET + 200));
    // And REST's spent alone.
    let mut meter = Meter::new("workers", "sam", 1000);
    meter.budget(&json!({"resources": {
        "core": {"remaining": 0, "reset": RESET + 100},
        "graphql": {"remaining": 10, "reset": RESET + 200},
    }}));
    assert_eq!(meter.until(1000), Some(RESET + 100));
}

#[test]
fn a_reading_after_the_reset_has_nothing_to_wait_for() {
    let mut meter = reading(1000, 0, RESET);
    meter.merge(reading(RESET + 1, 4999, RESET + 3600));
    assert_eq!(meter.until(RESET + 1), None);
}
