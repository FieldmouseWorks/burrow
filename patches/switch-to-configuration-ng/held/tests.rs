//! held.rs's own: which waits an activation lets be.

use super::*;

fn job(id: u32, unit: &str, kind: &str, state: &str) -> Job {
    Job {
        id,
        unit: unit.into(),
        kind: kind.into(),
        state: state.into(),
        path: format!("/org/freedesktop/systemd1/job/{id}"),
        unit_path: String::new(),
    }
}

/// What `held` lets be of `waited` (ids of `jobs`; 0 for one that has
/// ended), as (unit, run's unit) pairs, sorted; `None` when it waits.
fn let_be(
    jobs: &[Job],
    waited: &[u32],
    runs: &[u32],
    waits: &[(u32, &[u32])],
) -> Option<Vec<(String, String)>> {
    let by_id: HashMap<u32, Job> = jobs.iter().map(|j| (j.id, j.clone())).collect();
    let waits: HashMap<u32, Vec<u32>> = waits.iter().map(|(k, v)| (*k, v.to_vec())).collect();
    let waited: Vec<Option<Job>> = waited.iter().map(|id| by_id.get(id).cloned()).collect();
    let runs = runs.iter().copied().collect();
    let mut got: Vec<(String, String)> = held(&waited, &runs, |j| {
        waits
            .get(&j.id)
            .into_iter()
            .flatten()
            .map(|id| by_id[id].clone())
            .collect()
    })?
    .into_iter()
    .map(|(j, r)| (j.unit, r.unit))
    .collect();
    got.sort();
    Some(got)
}

fn pair(a: &str, b: &str) -> (String, String) {
    (a.into(), b.into())
}

#[test]
fn a_restart_behind_a_run_from_before_is_let_be() {
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(9, "renovate-sweep.service", "restart", "waiting"),
    ];
    assert_eq!(
        let_be(&jobs, &[9], &[1], &[(9, &[1])]),
        Some(vec![pair("renovate-sweep.service", "renovate.service")])
    );
}

#[test]
fn a_target_waiting_only_behind_the_run_is_let_be_with_what_it_waits_on() {
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(2, "renovate-sweep.service", "start", "waiting"),
        job(9, "multi-user.target", "start", "waiting"),
    ];
    assert_eq!(
        let_be(&jobs, &[9], &[1], &[(9, &[2]), (2, &[1])]),
        Some(vec![pair("multi-user.target", "renovate.service")])
    );
}

#[test]
fn it_waits_while_anything_it_waits_on_is_not_behind_the_run() {
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(9, "renovate-sweep.service", "restart", "waiting"),
        job(10, "app.service", "start", "running"),
    ];
    // The app's start runs: everything waited on is waited for.
    assert_eq!(let_be(&jobs, &[9, 10], &[1], &[(9, &[1])]), None);
    // A job that has ended has its result on the way: waited for.
    assert_eq!(let_be(&jobs, &[9, 0], &[1], &[(9, &[1])]), None);
    // Nothing waited on is nothing to let be.
    assert_eq!(let_be(&jobs, &[], &[1], &[]), None);
}

#[test]
fn what_the_activation_started_itself_is_still_waited_for() {
    // A new unit the target pulled in that hangs starting.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(10, "hung.service", "start", "running"),
        job(11, "multi-user.target", "start", "waiting"),
    ];
    assert_eq!(let_be(&jobs, &[11], &[1], &[(11, &[10])]), None);
}

#[test]
fn a_job_that_also_waits_on_something_else_is_waited_for() {
    // The target waits on the sweep behind the run and on a new unit that
    // hangs.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(2, "sweep.service", "start", "waiting"),
        job(10, "hung.service", "start", "running"),
        job(11, "multi-user.target", "start", "waiting"),
    ];
    assert_eq!(let_be(&jobs, &[11], &[1], &[(11, &[2, 10]), (2, &[1])]), None);
    // The sweep alone would be let be; with the target, it's all waited on.
    assert_eq!(
        let_be(&jobs, &[2], &[1], &[(11, &[2, 10]), (2, &[1])]),
        Some(vec![pair("sweep.service", "renovate.service")])
    );
    assert_eq!(let_be(&jobs, &[2, 11], &[1], &[(11, &[2, 10]), (2, &[1])]), None);
    // Through an intermediate waiting job, and when the other branch is a
    // waiting job with nothing it waits for.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(2, "sweep.service", "start", "waiting"),
        job(3, "mid.target", "start", "waiting"),
        job(10, "late.service", "start", "waiting"),
        job(11, "multi-user.target", "start", "waiting"),
    ];
    assert_eq!(
        let_be(&jobs, &[11], &[1], &[(11, &[3, 10]), (3, &[2]), (2, &[1])]),
        None
    );
}

#[test]
fn a_job_with_nothing_to_wait_for_or_unreadable_waits_is_waited_for() {
    // GetBefore that can't be read says nothing: waited for, as before.
    let jobs = [job(9, "sweep.service", "restart", "waiting")];
    assert_eq!(let_be(&jobs, &[9], &[1], &[]), None);
}

#[test]
fn a_waiting_job_that_started_later_is_not_a_run_from_before() {
    // Job 2 was waiting as the activation began and runs now: not a run
    // from before.
    let jobs = [
        job(2, "slow.service", "start", "running"),
        job(9, "app.service", "restart", "waiting"),
    ];
    assert_eq!(let_be(&jobs, &[9], &[], &[(9, &[2])]), None);
}

#[test]
fn a_stop_or_a_running_job_is_waited_for() {
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(3, "old.service", "stop", "waiting"),
        job(4, "app.service", "start", "running"),
    ];
    assert_eq!(let_be(&jobs, &[3], &[1], &[(3, &[1])]), None);
    assert_eq!(let_be(&jobs, &[4], &[1, 4], &[(4, &[1])]), None);
}

#[test]
fn a_wait_that_goes_round_ends() {
    let jobs = [
        job(5, "a.service", "start", "waiting"),
        job(6, "b.service", "start", "waiting"),
    ];
    assert_eq!(let_be(&jobs, &[5], &[], &[(5, &[6]), (6, &[5])]), None);
}

#[test]
fn only_a_start_of_a_known_invocation_is_the_same_run() {
    let then = Some(vec![7, 7]);
    assert!(same_run(then.as_ref(), "start", Some(vec![7, 7])));
    // A new run of the unit.
    assert!(!same_run(then.as_ref(), "start", Some(vec![8, 8])));
    // A start turned into something else.
    for kind in ["reload", "restart", "reload-or-start", "try-restart"] {
        assert!(!same_run(then.as_ref(), kind, Some(vec![7, 7])), "{kind}");
    }
    // An invocation that couldn't be read, or names none, is never the
    // same as another.
    assert!(!same_run(None, "start", None));
    assert!(!same_run(then.as_ref(), "start", None));
    assert_eq!(known(Some(vec![])), None);
    assert_eq!(known(Some(vec![0; 16])), None);
    assert!(!same_run(None, "start", Some(vec![0; 16])));
    assert_eq!(known(Some(vec![0, 3])), Some(vec![0, 3]));
}

#[test]
fn a_barrier_lets_nothing_be_and_only_while_it_waits() {
    assert!(!barrier());
    assert!(whole(barrier));
    assert!(whole(|| whole(barrier) && barrier()));
    assert!(!barrier());
}

#[test]
fn a_barrier_is_kept_whatever_its_wait_returns() {
    let failed: Result<(), &str> = whole(|| Err("bus gone"));
    assert!(failed.is_err());
    assert!(!barrier());
}
