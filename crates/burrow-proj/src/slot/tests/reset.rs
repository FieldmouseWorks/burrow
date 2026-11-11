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

#[test]
fn independent_pools_start_while_claude_census_is_blocked() {
    for codex in [false, true] {
        let tmp = TempDir::new().unwrap();
        let jobs = jobs_dir(&tmp);
        let census = File::create(jobs.join(".slots/.lock")).unwrap();
        census.lock().unwrap();
        let cfg = config(&jobs, 1);
        let _usage = crate::usage::lock(&cfg).unwrap();
        fs::create_dir_all(jobs.join("escape")).unwrap();
        if !codex {
            fs::write(jobs.join("escape/unstick"), "").unwrap();
        }
        let _kid = waiter_to(
            if codex { "codex" } else { "take" },
            &jobs,
            &tmp,
            "escape",
            1,
        );
        until("independent turn starts", || {
            took(&jobs, "escape").is_some()
        });
        assert_eq!(in_queue(&jobs), 0);
        assert_eq!(
            fs::read_to_string(jobs.join("escape/1.slot")).unwrap(),
            if codex { "codex-1\n" } else { "unstick\n" }
        );
    }
}

#[test]
fn queue_to_unstick_and_usage_retry_escape_a_blocked_census() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    let ordinary = File::create(jobs.join(".slots/1")).unwrap();
    ordinary.lock().unwrap();
    let _kid = waiter(&jobs, &tmp, "escape", 1);
    until("Claude waiter joined", || in_queue(&jobs) == 1);
    let census = File::create(jobs.join(".slots/.lock")).unwrap();
    census.lock().unwrap();
    out_of_usage(&jobs);
    fs::write(jobs.join("escape/unstick"), "").unwrap();
    until(
        "emergency waiter leaves Claude queue even during usage hold",
        || in_queue(&jobs) == 0,
    );
    assert_eq!(took(&jobs, "escape"), None);
    fs::remove_file(jobs.join(crate::usage::FILE)).unwrap();
    until("emergency retry starts despite blocked census", || {
        took(&jobs, "escape").is_some()
    });
}

#[test]
fn pipeline_holders_satisfy_only_their_own_usable_share() {
    use crate::pick::{Waiter, pick_budget};
    let mut cfg = config(Path::new("/unused"), 4);
    cfg.pipeline_slots = 1;
    let line: Vec<_> = [Class::Lead, Class::Pipeline, Class::Normal]
        .into_iter()
        .enumerate()
        .map(|(i, class)| Waiter {
            name: i.to_string(),
            class,
            project: i.to_string(),
        })
        .collect();
    for held_class in [Class::Lead, Class::Pipeline] {
        for held_slot in 0..4 {
            let mut slots = vec![Slot::Free; 4];
            slots[held_slot] = Slot::Held {
                class: Some(held_class),
                project: "held".into(),
            };
            let picked = pick_budget(&cfg, &slots, &line[..2], 1);
            assert_eq!(picked.len(), 1);
            assert_eq!(
                line[picked[0].0].class,
                if held_class == Class::Pipeline {
                    Class::Lead
                } else {
                    Class::Pipeline
                }
            );
        }
    }
    cfg.max_jobs = 2;
    for cap in [1, 2, 3, usize::MAX] {
        let slots = [
            Slot::Held {
                class: Some(Class::Pipeline),
                project: "held".into(),
            },
            Slot::Free,
        ];
        let picked = pick_budget(&cfg, &slots, &line[1..], cap);
        assert_eq!(
            picked,
            [(0, 2)],
            "unusable ordinary share must not block pipeline work"
        );
    }
}

#[test]
fn full_budgets_do_not_lose_progress_in_small_slot_configurations() {
    use crate::pick::{Waiter, pick_budget};
    // Enumerate holder classes and physical positions, including no general
    // slots, no reserved slots, and project caps of zero or one.
    let classes = [Class::Lead, Class::Pipeline, Class::Normal, Class::Steward];
    let line: Vec<_> = classes
        .into_iter()
        .enumerate()
        .map(|(i, class)| Waiter {
            name: i.to_string(),
            class,
            project: "one".into(),
        })
        .collect();
    for max in 1..=4 {
        for reserved in 0..=max {
            for per_project in 0..=2 {
                let mut cfg = config(Path::new("/unused"), max);
                cfg.pipeline_slots = reserved;
                cfg.max_jobs_per_project = per_project;
                for mask in 0..5usize.pow(max as u32) {
                    let mut value = mask;
                    let slots: Vec<_> = (0..max)
                        .map(|_| {
                            let v = value % 5;
                            value /= 5;
                            if v == 0 {
                                Slot::Free
                            } else {
                                Slot::Held {
                                    class: Some(classes[v - 1]),
                                    project: "one".into(),
                                }
                            }
                        })
                        .collect();
                    let full = pick_budget(&cfg, &slots, &line, usize::MAX);
                    assert_eq!(pick_budget(&cfg, &slots, &line, max), full);
                    for cap in 1..=max {
                        let picked = pick_budget(&cfg, &slots, &line, cap);
                        assert!(picked.len() <= cap);
                        assert_eq!(
                            picked.is_empty(),
                            full.is_empty(),
                            "lost progress: max={max} reserved={reserved} per_project={per_project} slots={slots:?} cap={cap}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn queue_birth_is_ordered_with_the_release_commit() {
    let tmp = TempDir::new().unwrap();
    let jobs = jobs_dir(&tmp);
    out_of_usage(&jobs);
    let cfg = config(&jobs, 1);
    let release = crate::usage::lock(&cfg).unwrap();
    let _kid = waiter(&jobs, &tmp, "joining", 1);
    until("waiter started", || {
        jobs.join("joining/1.withdrawable").exists()
    });
    sleep(Duration::from_secs(3));
    assert_eq!(
        in_queue(&jobs),
        0,
        "queue birth interleaved with the release transaction"
    );
    drop(release);
    until("waiter publishes after transaction", || {
        in_queue(&jobs) == 1
    });
}

#[test]
fn usage_contention_yields_to_emergency_promotion() {
    for published in [false, true] {
        for released_hold in [false, true] {
            let tmp = TempDir::new().unwrap();
            let jobs = jobs_dir(&tmp);
            let cfg = config(&jobs, 1);
            let ordinary = File::create(jobs.join(".slots/1")).unwrap();
            ordinary.lock().unwrap();
            if released_hold {
                out_of_usage(&jobs);
            }
            let mut kid = None;
            if published {
                kid = Some(waiter(&jobs, &tmp, "promoted", 1));
                until("waiter joined before contention", || in_queue(&jobs) == 1);
            }
            let usage = crate::usage::lock(&cfg).unwrap();
            if released_hold {
                // A committed release whose notification still owns the lock.
                let path = jobs.join(crate::usage::FILE);
                let mut state: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
                state["back"] = now().into();
                state["admission"] = crate::now_nanos().into();
                fs::write(path, serde_json::to_string(&state).unwrap()).unwrap();
            }
            if !published {
                kid = Some(waiter(&jobs, &tmp, "promoted", 1));
            }
            until("waiter is ready", || {
                jobs.join("promoted/1.withdrawable").exists()
            });
            sleep(Duration::from_secs(3));
            assert_eq!(in_queue(&jobs), usize::from(published));
            // Retrying a busy usage transaction must also release the census.
            let census = File::create(jobs.join(".slots/.lock")).unwrap();
            census
                .try_lock()
                .expect("usage contention retained census lock");
            fs::write(jobs.join("promoted/unstick"), "").unwrap();
            until("promoted waiter escapes usage contention", || {
                took(&jobs, "promoted").is_some()
            });
            assert_eq!(
                fs::read_to_string(jobs.join("promoted/1.slot")).unwrap(),
                "unstick\n"
            );
            assert_eq!(in_queue(&jobs), 0);
            drop(kid);
            drop(usage);
        }
    }
}

#[test]
fn uncapped_physical_reservations_survive_holder_accounting() {
    use crate::pick::{Waiter, pick_budget};
    let mut cfg = config(Path::new("/unused"), 4);
    cfg.pipeline_slots = 1;
    let slots = [
        Slot::Held {
            class: Some(Class::Pipeline),
            project: "pipeline".into(),
        },
        Slot::Held {
            class: Some(Class::Lead),
            project: "lead".into(),
        },
        Slot::Free,
        Slot::Free,
    ];
    let line: Vec<_> = [Class::Lead, Class::Pipeline, Class::Normal]
        .into_iter()
        .enumerate()
        .map(|(i, class)| Waiter {
            name: i.to_string(),
            class,
            project: i.to_string(),
        })
        .collect();
    for budget in [1, 2, 4, usize::MAX] {
        let picked = pick_budget(&cfg, &slots, &line, budget);
        assert!(
            !picked.iter().any(|(i, _)| *i == 0),
            "lead consumed a physical reservation"
        );
        assert!(picked.iter().any(|(i, _)| *i == 2), "ordinary share lost");
        if budget >= 2 {
            assert_eq!(picked, [(1, 4), (2, 3)]);
        }
    }
    // A satisfied pipeline share does not reserve a second admission from
    // the reset budget when lead work can use a general slot instead.
    let mut slots = slots.to_vec();
    slots[1] = Slot::Free;
    for _ in 0..3 {
        let picked = pick_budget(&cfg, &slots, &line[..2], 1);
        assert_eq!(picked, [(0, 2)]);
        slots[1] = Slot::Held {
            class: Some(Class::Lead),
            project: "lead".into(),
        };
        assert!(pick_budget(&cfg, &slots, &line[..2], 0).is_empty());
        slots[1] = Slot::Free;
    }
}

#[test]
fn pipeline_budget_survives_ordinary_queue_order_and_repeated_admissions() {
    use crate::pick::{Waiter, pick_budget};
    let mut cfg = config(Path::new("/unused"), 4);
    cfg.pipeline_slots = 1;
    cfg.max_jobs_per_project = 2;
    // A census can observe a new class before its filename is renamed:
    // publication during a hold, class changes and maxWait promotion all
    // permit pipeline work anywhere in filename order.
    for position in 0..4 {
        let mut classes = vec![Class::Normal, Class::First, Class::Steward];
        classes.insert(position, Class::Pipeline);
        let line: Vec<_> = classes
            .into_iter()
            .enumerate()
            .map(|(i, class)| Waiter {
                name: format!("{i}-unrenamed"),
                class,
                project: i.to_string(),
            })
            .collect();
        let slots = vec![Slot::Free; 4];
        let picked = pick_budget(&cfg, &slots, &line, 2);
        assert_eq!(picked.len(), 2);
        assert!(
            picked
                .iter()
                .any(|(i, _)| line[*i].class == Class::Pipeline)
        );
        assert!(picked.iter().any(|(i, _)| !line[*i].class.pipeline()));
        for &(first, slot) in &picked {
            let mut slots = slots.clone();
            slots[slot - 1] = Slot::Held {
                class: Some(line[first].class),
                project: line[first].project.clone(),
            };
            let rest: Vec<_> = line
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != first)
                .map(|(_, w)| w.clone())
                .collect();
            let next = pick_budget(&cfg, &slots, &rest, 1);
            assert_eq!(next.len(), 1);
            assert_eq!(
                rest[next[0].0].class == Class::Pipeline,
                line[first].class != Class::Pipeline
            );
        }
        // A pipeline holder satisfies the budget reservation in every slot;
        // lead holders cannot satisfy it. Ordinary work keeps its usable share.
        for at in 0..4 {
            for class in [Class::Pipeline, Class::Lead, Class::Normal] {
                let mut slots = slots.clone();
                slots[at] = Slot::Held {
                    class: Some(class),
                    project: "held".into(),
                };
                let cap = if class == Class::Lead { 2 } else { 1 };
                let next = pick_budget(&cfg, &slots, &line, cap);
                assert_eq!(next.len(), cap);
                assert_eq!(
                    next.iter().any(|(i, _)| line[*i].class == Class::Pipeline),
                    class != Class::Pipeline
                );
            }
        }
        assert_eq!(
            pick_budget(&cfg, &slots, &line, 4),
            pick_budget(&cfg, &slots, &line, usize::MAX)
        );
    }
}

#[test]
fn ordinary_caps_and_multiple_pipeline_reservations_share_the_budget() {
    use crate::pick::{Waiter, pick_budget};
    let mut cfg = config(Path::new("/unused"), 6);
    cfg.pipeline_slots = 2;
    cfg.max_jobs_per_project = 2;
    let line: Vec<_> = [
        Class::Normal,
        Class::Normal,
        Class::Steward,
        Class::Pipeline,
        Class::Pipeline,
    ]
    .into_iter()
    .enumerate()
    .map(|(i, class)| Waiter {
        name: i.to_string(),
        class,
        project: "one".into(),
    })
    .collect();
    let mut slots = vec![Slot::Free; 6];
    let picked = pick_budget(&cfg, &slots, &line, 3);
    assert_eq!(
        picked.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        [0, 3, 4]
    );
    slots[0] = Slot::Held {
        class: Some(Class::Normal),
        project: "one".into(),
    };
    let picked = pick_budget(&cfg, &slots, &line, 3);
    assert_eq!(
        picked.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
        [2, 3, 4]
    );
    slots[1] = Slot::Held {
        class: Some(Class::Steward),
        project: "one".into(),
    };
    let picked = pick_budget(&cfg, &slots, &line, 3);
    assert_eq!(picked.iter().map(|(i, _)| *i).collect::<Vec<_>>(), [3, 4]);
}

#[test]
fn nonbinding_budgets_preserve_ordinary_share_after_leads_take_general_slots() {
    use crate::pick::{Waiter, pick_budget};
    let mut cfg = config(Path::new("/unused"), 4);
    cfg.pipeline_slots = 1;
    // Pipeline work cannot use the occupied reserved slot, and the leads
    // exhaust its general allowance. Ordinary work must still use slot 3.
    for held_class in [Class::Lead, Class::Pipeline] {
        for pipeline_first in [false, true] {
            let mut classes = vec![Class::Lead, Class::Lead];
            classes.extend(if pipeline_first {
                [Class::Pipeline, Class::Normal]
            } else {
                [Class::Normal, Class::Pipeline]
            });
            let line: Vec<_> = classes
                .into_iter()
                .enumerate()
                .map(|(i, class)| Waiter {
                    name: i.to_string(),
                    class,
                    project: i.to_string(),
                })
                .collect();
            let slots = [
                Slot::Free,
                Slot::Free,
                Slot::Free,
                Slot::Held {
                    class: Some(held_class),
                    project: "held".into(),
                },
            ];
            let normal = line.iter().position(|w| w.class == Class::Normal).unwrap();
            for budget in [3, 4, usize::MAX] {
                assert_eq!(
                    pick_budget(&cfg, &slots, &line, budget),
                    [(0, 1), (1, 2), (normal, 3)]
                );
            }
        }
    }
}
