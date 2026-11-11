use super::*;

fn admission_cutoff(cfg: &Config) -> Option<i64> {
    super::super::admission_cutoff(cfg).unwrap()
}

#[test]
fn admission_cutoff_survives_natural_completion_and_settlement() {
    let (_d, cfg) = rig();
    let t = now() - 100;
    record(&cfg, "web", Some(t + 10), SAID, t).unwrap();
    let cutoff = (t + 10) * 1_000_000_000;
    assert_eq!(admission_cutoff(&cfg), Some(cutoff));
    back(&cfg, Some(t + 20));
    assert_eq!(admission_cutoff(&cfg), Some(cutoff));
    let s = kept(&cfg).unwrap();
    settle(&cfg, s.clone(), s.release() + cfg.stall_queued() + 1);
    assert!(kept(&cfg).is_none());
    assert_eq!(admission_cutoff(&cfg), Some(cutoff));
    back(&cfg, None);
    assert_eq!(admission_cutoff(&cfg), Some(cutoff));
}

#[test]
fn early_admission_cutoff_has_subsecond_order_and_never_expands() {
    let (_d, cfg) = rig();
    let t = now();
    record(&cfg, "web", Some(t + 3600), SAID, t - 100).unwrap();
    let before = crate::now_nanos();
    back(&cfg, None);
    let after = crate::now_nanos();
    let cutoff = admission_cutoff(&cfg).unwrap();
    assert!(before <= cutoff && cutoff <= after);
    back(&cfg, Some(t + 1));
    assert_eq!(admission_cutoff(&cfg), Some(cutoff));
    let s = kept(&cfg).unwrap();
    settle(&cfg, s.clone(), s.release() + cfg.stall_queued() + 1);
    assert_eq!(admission_cutoff(&cfg), Some(cutoff));
}

#[test]
fn failed_release_writes_leave_the_natural_cohort_intact_and_retry_precisely() {
    for boundary in [".usage.released.new", ".usage.json.new"] {
        let (_d, cfg) = rig();
        let t = now();
        record(&cfg, "web", Some(t + 3600), SAID, t - 100).unwrap();
        let block = cfg.jobs.join(boundary);
        fs::create_dir(&block).unwrap();
        back(&cfg, None);
        let s = kept(&cfg).unwrap();
        assert!(s.back.is_none());
        assert_eq!(admission_cutoff(&cfg), None);
        // When this unchanged hold expires, all arrivals through its actual
        // reset must be included, not just those preceding the failed attempt.
        assert!(!s.holds(s.until));
        assert_eq!(
            admission_release(&cfg, &s).unwrap(),
            s.until * 1_000_000_000
        );
        fs::remove_dir(block).unwrap();
        let before = crate::now_nanos();
        back(&cfg, None);
        let cutoff = admission_cutoff(&cfg).unwrap();
        assert!(cutoff >= before && cutoff <= crate::now_nanos());
        assert_eq!(kept(&cfg).unwrap().admission, Some(cutoff));
    }
}

#[test]
fn interrupted_release_watermarks_cannot_override_active_or_extended_holds() {
    let (_d, cfg) = rig();
    let t = now();
    record(&cfg, "web", Some(t + 3600), SAID, t - 100).unwrap();
    // Crash after the old cutoff write, or after the refusal watermark write,
    // but before the atomic record replacement. Neither commits `back`.
    for marked in [false, true] {
        let s = kept(&cfg).unwrap();
        fs::write(
            cfg.jobs.join(".usage.admission"),
            format!("{}:{}\t{}", s.at, s.until, (t - 1) * 1_000_000_000),
        )
        .unwrap();
        if marked {
            mark(&cfg, t).unwrap();
            // Crash after writing the staged record but before its rename:
            // only the original committed FILE may decide the cohort.
            let mut staged = s.clone();
            staged.back = Some(t);
            staged.admission = Some(crate::now_nanos());
            fs::write(
                cfg.jobs.join(format!("{FILE}.new")),
                serde_json::to_string(&staged).unwrap(),
            )
            .unwrap();
            assert_eq!(kept(&cfg), Some(s.clone()));
        }
        assert_eq!(admission_cutoff(&cfg), None);
        assert_eq!(
            admission_release(&cfg, &s).unwrap(),
            s.until * 1_000_000_000
        );
    }
    record(&cfg, "later", Some(t + 7200), SAID, t).unwrap();
    let extended = kept(&cfg).unwrap();
    assert_eq!(
        admission_release(&cfg, &extended).unwrap(),
        (t + 7200) * 1_000_000_000
    );
    back(&cfg, None);
    let cutoff = admission_cutoff(&cfg).unwrap();
    let old = kept(&cfg).unwrap();
    // A failed settlement cannot remove the only committed precise boundary.
    let block = cfg.jobs.join(".usage.admission.new");
    fs::create_dir(&block).unwrap();
    settle(&cfg, old.clone(), old.release() + cfg.stall_queued() + 1);
    assert_eq!(kept(&cfg), Some(old.clone()));
    assert_eq!(admission_cutoff(&cfg), Some(cutoff));
    fs::remove_dir(block).unwrap();
    settle(&cfg, old.clone(), old.release() + cfg.stall_queued() + 1);
    assert!(kept(&cfg).is_none());
    assert_eq!(admission_cutoff(&cfg), Some(cutoff));
    record(&cfg, "replacement", Some(t + 10800), SAID, t + 1).unwrap();
    let replacement = kept(&cfg).unwrap();
    assert_eq!(admission_cutoff(&cfg), None);
    assert_eq!(
        admission_release(&cfg, &replacement).unwrap(),
        replacement.until * 1_000_000_000
    );
}
