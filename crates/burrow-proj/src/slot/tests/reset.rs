use super::*;

#[test]
fn usage_reset_trickles_real_waiters_under_the_admission_lock() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    out_of_usage(&jobs);
    let mut kids = waiters(&jobs, &tmp, &["w0", "w1", "w2", "w3"], 6);
    fs::write(
        jobs.join(".usage.admission"),
        format!("1:2\t{}", crate::now_nanos()),
    )
    .unwrap();
    fs::remove_file(jobs.join(crate::usage::FILE)).unwrap();
    until("two reset turns take slots", || {
        took(&jobs, "w0").is_some() && took(&jobs, "w1").is_some()
    });
    sleep(Duration::from_secs(4));
    assert_eq!(took(&jobs, "w2"), None, "reset admitted a third turn");
    assert_eq!(took(&jobs, "w3"), None, "reset admitted a fourth turn");
    kids.remove(0);
    until("the next reset turn replaces a finished turn", || {
        took(&jobs, "w2").is_some()
    });
    assert_eq!(took(&jobs, "w3"), None);
    kids.remove(0);
    until("the last queued reset turn progresses", || {
        took(&jobs, "w3").is_some()
    });
}

#[test]
fn capped_selection_preserves_shares_and_project_limits() {
    use crate::pick::{Waiter, pick_budget};
    let mut cfg = config(Path::new("/unused"), 4);
    cfg.pipeline_slots = 1;
    cfg.max_jobs_per_project = 2;
    let line: Vec<_> = [
        Class::Lead,
        Class::Lead,
        Class::Pipeline,
        Class::Normal,
        Class::Normal,
        Class::Steward,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, class)| Waiter {
        name: i.to_string(),
        class,
        project: if class.pipeline() {
            i.to_string()
        } else {
            "ordinary".into()
        },
    })
    .collect();
    let mut slots = vec![Slot::Free; 4];
    let selected = pick_budget(&cfg, &slots, &line, 2);
    assert_eq!(selected.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [2, 3]);
    // Each actual admission is followed by a new look; either reserved share
    // may acquire first without letting the lead backlog consume the other.
    for &(first, slot) in &selected {
        slots[slot - 1] = Slot::Held {
            class: Some(line[first].class),
            project: line[first].project.clone(),
        };
        let remaining: Vec<_> = line
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != first)
            .map(|(_, w)| w.clone())
            .collect();
        let next = pick_budget(&cfg, &slots, &remaining, 1);
        assert_eq!(next.len(), 1);
        assert_eq!(
            remaining[next[0].0].class,
            if first == 2 {
                Class::Normal
            } else {
                Class::Pipeline
            }
        );
        assert!(pick_budget(&cfg, &slots, &remaining, 0).is_empty());
        slots[slot - 1] = Slot::Free;
    }
    slots[0] = Slot::Held {
        class: Some(Class::Normal),
        project: "ordinary".into(),
    };
    let ordinary = &line[3..];
    let selected = pick_budget(&cfg, &slots, ordinary, 1);
    assert_eq!(ordinary[selected[0].0].class, Class::Steward);
    slots[1] = Slot::Held {
        class: Some(Class::Steward),
        project: "ordinary".into(),
    };
    assert!(pick_budget(&cfg, &slots, ordinary, 1).is_empty());
    slots = vec![Slot::Free; 4];
    assert_eq!(pick_budget(&cfg, &slots, &line, usize::MAX).len(), 4);
}

#[test]
fn queue_identity_changes_wait_for_the_census_lock() {
    for promotion in [true, false] {
        let tmp = TempDir::new().unwrap();
        let jobs = jobs_dir(&tmp);
        let slot = File::create(jobs.join(".slots/1")).unwrap();
        slot.lock().unwrap();
        let _kid = waiter_to(
            if promotion { "soon" } else { "take" },
            &jobs,
            &tmp,
            "w0",
            1,
        );
        until("waiter published", || in_queue(&jobs) == 1);
        let queue = jobs.join(".queue");
        let old = fs::read_dir(&queue)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let made = fs::metadata(&old).unwrap().modified().unwrap();
        let census = File::create(jobs.join(".slots/.lock")).unwrap();
        census.lock().unwrap();
        if !promotion {
            fs::write(jobs.join("w0/pipeline"), "").unwrap();
        }
        sleep(Duration::from_secs(6));
        assert!(old.exists(), "queue renamed during the census");
        census.unlock().unwrap();
        until("queue identity changed", || !old.exists());
        let new = fs::read_dir(&queue)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(fs::metadata(new).unwrap().modified().unwrap(), made);
    }
}

#[test]
fn reset_backlog_releases_only_the_available_trickle_share() {
    use crate::pick::{Waiter, pick, pick_budget};
    let mut cfg: Config = serde_json::from_value(example()).unwrap();
    cfg.max_jobs = 6;
    cfg.pipeline_slots = 1;
    cfg.max_jobs_per_project = 4;
    let line: Vec<_> = (0..6)
        .map(|i| Waiter {
            name: format!("job-{i}"),
            class: Class::Normal,
            project: format!("project-{i}"),
        })
        .collect();
    let mut slots = vec![Slot::Free; 6];
    let selected = pick(&cfg, &slots, &line);
    assert_eq!(selected.len(), 5, "ordinary admission would burst at reset");
    let selected = pick_budget(&cfg, &slots, &line, 2);
    assert_eq!(selected.len(), 2);
    let first = selected[0];
    slots[first.1 - 1] = Slot::Held {
        class: Some(Class::Normal),
        project: line[first.0].project.clone(),
    };
    let selected = pick_budget(&cfg, &slots, &line[1..], 1);
    assert_eq!(
        selected.len(),
        1,
        "another waiter must count the first admission"
    );
    slots[selected[0].1 - 1] = Slot::Held {
        class: Some(Class::Pipeline),
        project: "pipeline".into(),
    };
    let selected = pick_budget(&cfg, &slots, &line[2..], 0);
    assert!(
        selected.is_empty(),
        "pipeline work also consumes reset capacity"
    );
    let selected = pick(&cfg, &slots, &line[2..]);
    let ordinary = selected.clone();
    let selected = pick_budget(&cfg, &slots, &line[2..], usize::MAX);
    assert_eq!(
        selected, ordinary,
        "normal admission returns after the backlog drains"
    );
}

#[test]
fn cohort_uses_precise_birth_times_and_survives_renames() {
    use crate::pick::Waiter;
    let tmp = TempDir::new().unwrap();
    let queue = tmp.path();
    let cutoff = 1_700_000_000_500_000_000i64;
    let mut line: Vec<_> = [("2-1-old", cutoff - 1), ("2-2-new", cutoff + 1)]
        .into_iter()
        .map(|(name, at)| {
            let file = File::create(queue.join(name)).unwrap();
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(UNIX_EPOCH + Duration::from_nanos(at as u64)),
            )
            .unwrap();
            Waiter {
                name: name.into(),
                class: Class::Normal,
                project: "same".into(),
            }
        })
        .collect();
    assert!(super::super::draining(queue, &line, cutoff));
    assert!(
        !super::super::draining(queue, &line[1..], cutoff),
        "later arrival in the release second is not backlog"
    );
    for name in ["1-1-old", "0-1-old", "2-1-old"] {
        fs::rename(queue.join(&line[0].name), queue.join(name)).unwrap();
        line[0].name = name.into();
        assert!(super::super::draining(queue, &line, cutoff));
    }
    fs::remove_file(queue.join(&line[0].name)).unwrap();
    assert!(
        super::super::draining(queue, &line, cutoff),
        "missing stale path cannot prove drainage"
    );
    line.remove(0);
    assert!(!super::super::draining(queue, &line, cutoff));
}
