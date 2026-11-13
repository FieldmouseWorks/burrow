//! queue.rs's own: what it lets go, and its recovery list.

use super::*;

fn job(id: u32, unit: &str, kind: &str, state: &str) -> Job {
    Job(
        id,
        unit.into(),
        kind.into(),
        state.into(),
        format!("/org/freedesktop/systemd1/job/{id}"),
        String::new(),
    )
}

fn set<T: Clone + Eq + std::hash::Hash>(items: &[T]) -> HashSet<T> {
    items.iter().cloned().collect()
}

/// What `held` finds, as (held unit, run's unit) pairs. `existing` are
/// the ids there were as the activation began, `running` the running ones.
fn found(
    jobs: &[Job],
    touched: &[&str],
    existing: &[u32],
    running: &[u32],
    waits: &[(u32, &[u32])],
) -> Vec<(String, String)> {
    let by_id: HashMap<u32, Job> = jobs.iter().map(|j| (j.id(), j.clone())).collect();
    let waits: HashMap<u32, Vec<u32>> = waits.iter().map(|(k, v)| (*k, v.to_vec())).collect();
    let touched = touched.iter().map(|t| t.to_string()).collect();
    held(jobs, &touched, &set(existing), &set(running), |j| {
        waits
            .get(&j.id())
            .into_iter()
            .flatten()
            .map(|id| by_id[id].clone())
            .collect()
    })
    .into_iter()
    .map(|(j, r)| (j.unit().to_string(), r.unit().to_string()))
    .collect()
}

fn sweep_behind_a_run() -> [Job; 2] {
    [
        job(1, "renovate.service", "start", "running"),
        job(9, "renovate-sweep.service", "restart", "waiting"),
    ]
}

#[test]
fn a_restart_behind_a_run_from_before_the_activation_is_held() {
    assert_eq!(
        found(
            &sweep_behind_a_run(),
            &["renovate-sweep.service"],
            &[1],
            &[1],
            &[(9, &[1])]
        ),
        [("renovate-sweep.service".into(), "renovate.service".into())]
    );
}

#[test]
fn a_target_waiting_only_behind_the_run_is_let_go_with_what_it_waits_on() {
    // multi-user.target waits on a sweep queued before the switch,
    // which waits on the run.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(2, "renovate-sweep.service", "start", "waiting"),
        job(9, "multi-user.target", "start", "waiting"),
    ];
    let mut got = found(
        &jobs,
        &["multi-user.target", "renovate-sweep.service"],
        &[1, 2],
        &[1],
        &[(9, &[2]), (2, &[1])],
    );
    got.sort();
    assert_eq!(
        got,
        [
            ("multi-user.target".into(), "renovate.service".into()),
            ("renovate-sweep.service".into(), "renovate.service".into())
        ]
    );
}

#[test]
fn a_job_the_activation_queued_behind_the_run_is_let_go_though_not_listed() {
    // `pass` came with the target, which waits on it: not listed, a job
    // of this activation.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(10, "pass.service", "start", "waiting"),
        job(11, "multi-user.target", "start", "waiting"),
    ];
    let mut got = found(
        &jobs,
        &["multi-user.target"],
        &[1],
        &[1],
        &[(11, &[10]), (10, &[1])],
    );
    got.sort();
    assert_eq!(
        got,
        [
            ("multi-user.target".into(), "renovate.service".into()),
            ("pass.service".into(), "renovate.service".into())
        ]
    );
    // One that was queued before, and isn't listed, is someone else's.
    assert_eq!(
        found(
            &jobs,
            &["multi-user.target"],
            &[1, 10],
            &[1],
            &[(11, &[10]), (10, &[1])]
        ),
        [("multi-user.target".into(), "renovate.service".into())]
    );
}

#[test]
fn a_new_job_nothing_the_activation_waits_on_waits_for_is_left_be() {
    // `systemctl start unrelated.service` while the switch runs: new,
    // behind the run, and nothing listed waits for it.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(10, "unrelated.service", "start", "waiting"),
        job(11, "multi-user.target", "start", "running"),
    ];
    assert!(found(&jobs, &["multi-user.target"], &[1], &[1], &[(10, &[1])]).is_empty());
    // Nor through a job that's no one's either.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(10, "unrelated.service", "start", "waiting"),
        job(12, "other.target", "start", "waiting"),
    ];
    assert!(
        found(
            &jobs,
            &["multi-user.target"],
            &[1],
            &[1],
            &[(12, &[10]), (10, &[1])]
        )
        .is_empty()
    );
}

#[test]
fn only_a_start_of_a_known_invocation_is_the_same_run() {
    let then = Some(vec![7, 7]);
    assert!(same_run(then.as_ref(), "start", Some(vec![7, 7])));
    // A new run of the unit.
    assert!(!same_run(then.as_ref(), "start", Some(vec![8, 8])));
    // A reload rerun under the same id, kind and invocation, or a start
    // turned into something else.
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
fn the_recovery_list_keeps_a_unit_until_it_is_taken_out() {
    let dir = tempfile::tempdir().unwrap();
    let list = dir.path().join("activation-restart-list");
    // Put in when it isn't there, the file made if need be.
    assert!(record(&list, "sweep.service").unwrap());
    assert_eq!(fs::read_to_string(&list).unwrap(), "sweep.service\n");
    // Already there (another's): not put in twice, and the caller
    // leaves it be.
    assert!(!record(&list, "sweep.service").unwrap());
    fs::write(&list, "a.service\nsweep.service\nb.service\n").unwrap();
    unrecord(&list, "sweep.service").unwrap();
    assert_eq!(fs::read_to_string(&list).unwrap(), "a.service\nb.service\n");
    // A list that can't be written keeps the job from being let go.
    assert!(record(&dir.path().join("none/list"), "sweep.service").is_err());
}

#[test]
fn what_the_activation_started_itself_is_still_waited_for() {
    // A new unit the target pulled in that hangs starting: the
    // switch reverts, as it should.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(10, "hung.service", "start", "running"),
        job(11, "multi-user.target", "start", "waiting"),
    ];
    assert!(found(&jobs, &["multi-user.target"], &[1], &[1], &[(11, &[10])]).is_empty());
}

#[test]
fn a_job_that_also_waits_on_something_else_is_kept_whole() {
    // The target waits on the sweep behind the run and on a new unit
    // that hangs: the target keeps both waits, the sweep alone goes.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(2, "sweep.service", "start", "waiting"),
        job(10, "hung.service", "start", "running"),
        job(11, "multi-user.target", "start", "waiting"),
    ];
    assert_eq!(
        found(
            &jobs,
            &["multi-user.target", "sweep.service"],
            &[1, 2],
            &[1],
            &[(11, &[2, 10]), (2, &[1])]
        ),
        [("sweep.service".into(), "renovate.service".into())]
    );
    // Through an intermediate waiting job, too, and when the other
    // branch is a job with nothing it could be waiting for.
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        job(2, "sweep.service", "start", "waiting"),
        job(3, "mid.target", "start", "waiting"),
        job(10, "late.service", "start", "waiting"),
        job(11, "multi-user.target", "start", "waiting"),
    ];
    let got = found(
        &jobs,
        &["multi-user.target", "sweep.service"],
        &[1, 2, 3],
        &[1],
        &[(11, &[3, 10]), (3, &[2]), (2, &[1])],
    );
    assert_eq!(got, [("sweep.service".into(), "renovate.service".into())]);
}

#[test]
fn a_waiting_job_that_started_later_is_not_a_run_from_before() {
    // Job 2 was queued before the activation and waiting; it runs now
    // with the new configuration. It isn't a run to let go behind.
    let jobs = [
        job(2, "slow.service", "start", "running"),
        job(9, "app.service", "restart", "waiting"),
    ];
    assert!(found(&jobs, &["app.service"], &[2, 9], &[], &[(9, &[2])]).is_empty());
}

#[test]
fn only_the_activations_own_work_and_kinds_are_let_go() {
    let jobs = [
        job(1, "renovate.service", "start", "running"),
        // Someone else's, queued behind it.
        job(2, "other.service", "start", "waiting"),
        // Not a kind the activation queues.
        job(3, "sweep.service", "verify-active", "waiting"),
        // Running, not waiting.
        job(4, "app.service", "start", "running"),
    ];
    assert!(
        found(
            &jobs,
            &["sweep.service", "app.service"],
            &[1, 2, 3, 4],
            &[1, 4],
            &[(2, &[1]), (3, &[1])]
        )
        .is_empty()
    );
}

#[test]
fn a_wait_that_goes_round_ends() {
    let jobs = [
        job(5, "a.service", "start", "waiting"),
        job(6, "b.service", "start", "waiting"),
    ];
    assert!(found(&jobs, &["a.service"], &[5, 6], &[], &[(5, &[6]), (6, &[5])]).is_empty());
}

#[test]
fn only_targets_may_follow_a_job_let_go() {
    let sweep = job(2, "sweep.service", "start", "waiting");
    let all = [
        job(3, "multi-user.target", "start", "waiting"),
        job(4, "consumer.service", "start", "waiting"),
        job(5, "late.target", "start", "waiting"),
    ];
    let follow_err = |_: &Job| -> Option<Vec<Job>> { None };
    let follow = |ids: &'static [(u32, &'static [usize])]| {
        let all = all.clone();
        move |j: &Job| {
            ids.iter()
                .filter(|(k, _)| *k == j.id())
                .flat_map(|(_, v)| v.iter().map(|i| all[*i].clone()))
                .collect::<Vec<_>>()
                .into()
        }
    };
    // A follower list that can't be read keeps the job.
    assert!(!only_targets_follow(&sweep, follow_err));
    assert!(only_targets_follow(&sweep, follow(&[(2, &[0])])));
    assert!(only_targets_follow(&sweep, follow(&[])));
    // A service ordered after it would run before it, in the gap.
    assert!(!only_targets_follow(&sweep, follow(&[(2, &[0, 1])])));
    // Through a target to a service.
    assert!(!only_targets_follow(
        &sweep,
        follow(&[(2, &[2]), (5, &[1])])
    ));
}

#[test]
fn busctl_says_a_call_returned_its_one_value() {
    let jobs: Option<Vec<Job>> = reply(
        br#"{"type":"a(usssoo)","data":[[[7,"a.service","start","waiting","/org/freedesktop/systemd1/job/7","/u"]]]}"#,
    );
    assert_eq!(
        jobs,
        Some(vec![job(7, "a.service", "start", "waiting")]).map(|mut v| {
            v[0].5 = "/u".into();
            v
        })
    );
    let none: Option<Vec<Job>> = reply(br#"{"type":"a(usssoo)","data":[[]]}"#);
    assert_eq!(none, Some(vec![]));
    assert_eq!(reply::<()>(b""), Some(()));
    assert_eq!(reply::<Vec<Job>>(b"nonsense"), None);
    let path: Option<String> =
        reply(br#"{"type":"o","data":["/org/freedesktop/systemd1/job/42"]}"#);
    assert_eq!(path.as_deref().and_then(job_id), Some(42));
    assert_eq!(job_id("/org/freedesktop/systemd1/unit/x"), None);
}
