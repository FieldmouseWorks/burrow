use super::*;

fn notifier(dir: &tempfile::TempDir, cfg: &mut Config) {
    burrow_test::executable(
        cfg.notify.as_ref().unwrap(),
        format!(
            r#"#!/bin/sh
cd '{}'
printf '%s|%s|%s\n' "$1" "$2" "$4" >> calls
case "$1" in
ensure) touch "remote-$2"; rm -f "pending-$2" ;;
close) test -e fail-close && touch "pending-$2" && exit 0
       rm -f "remote-$2" "pending-$2" ;;
pending) test -e fail-pending && exit 2
         test -e "pending-$2" ;;
esac
"#,
            dir.path().display()
        ),
    );
}

#[test]
fn migration_closes_legacy_and_interrupted_notices_without_sent_metadata() {
    for interrupted in [false, true] {
        let (dir, mut cfg) = fixture();
        notifier(&dir, &mut cfg);
        asks(&cfg, "leader", None, false);
        asks(&cfg, "follower", None, false);
        let follower = Job::new(&cfg, "follower");
        fs::write(
            host_path(&Job::new(&cfg, "leader"), "question-since"),
            "9999999999",
        )
        .unwrap();
        if interrupted {
            // ensure reached the remote, then died before any local delivery marker.
            fs::write(dir.path().join("remote-proj-question-follower-1"), "").unwrap();
        } else {
            notified(&cfg, &["leader", "follower"]);
            let _ = fs::remove_file(follower.turn("1", "question-sent"));
            // The earlier implementation could hide without confirming closure.
            fs::write(follower.turn("1", "question-hidden"), "hidden\n").unwrap();
        }
        merge(&cfg, "subject", &["leader".into(), "follower".into()]).unwrap();
        fs::write(dir.path().join("fail-close"), "").unwrap();
        notified(&cfg, &["follower"]);
        assert!(dir.path().join("remote-proj-question-follower-1").exists());
        assert_ne!(
            read(&follower.turn("1", "question-hidden")).as_deref(),
            Some("closed")
        );
        fs::remove_file(dir.path().join("fail-close")).unwrap();
        notified(&cfg, &["follower"]);
        assert!(!dir.path().join("remote-proj-question-follower-1").exists());
        assert_eq!(
            read(&follower.turn("1", "question-hidden")).as_deref(),
            Some("closed")
        );
        let closes = calls(&dir)
            .matches("close|proj-question-follower-1")
            .count();
        notified(&cfg, &["follower"]);
        assert_eq!(
            calls(&dir)
                .matches("close|proj-question-follower-1")
                .count(),
            closes
        );
    }
}

#[test]
fn hide_marker_and_pending_failures_keep_closure_retryable_then_allow_promotion() {
    for blocked in ["hidden", "pending"] {
        let (dir, mut cfg) = fixture();
        notifier(&dir, &mut cfg);
        for id in ["leader", "follower"] {
            asks(&cfg, id, None, false);
        }
        fs::write(
            host_path(&Job::new(&cfg, "leader"), "question-since"),
            "9999999999",
        )
        .unwrap();
        notified(&cfg, &["leader", "follower"]);
        merge(&cfg, "subject", &["leader".into(), "follower".into()]).unwrap();
        let follower = Job::new(&cfg, "follower");
        let hidden = follower.turn("1", "question-hidden");
        if blocked == "hidden" {
            fs::create_dir(&hidden).unwrap();
        } else {
            fs::write(dir.path().join("fail-pending"), "").unwrap();
        }
        let _ = crate::question::notify(&cfg, &follower);
        assert_ne!(read(&hidden).as_deref(), Some("closed"));
        if blocked == "hidden" {
            fs::remove_dir(&hidden).unwrap();
        } else {
            fs::remove_file(dir.path().join("fail-pending")).unwrap();
        }
        notified(&cfg, &["follower"]);
        assert!(hidden.exists());
        crate::cmd::stop(&cfg, "leader", false).unwrap();
        notified(&cfg, &["follower"]);
        assert!(!hidden.exists());
        assert!(dir.path().join("remote-proj-question-follower-1").exists());
        assert!(!follower.turn("1", "question-close").exists());
    }
}

#[test]
fn promotion_while_close_is_pending_reopens_one_notice_without_retiring_the_ask() {
    let (dir, mut cfg) = fixture();
    notifier(&dir, &mut cfg);
    for id in ["leader", "follower"] {
        asks(&cfg, id, None, false);
    }
    fs::write(
        host_path(&Job::new(&cfg, "leader"), "question-since"),
        "9999999999",
    )
    .unwrap();
    notified(&cfg, &["leader", "follower"]);
    merge(&cfg, "subject", &["leader".into(), "follower".into()]).unwrap();
    fs::write(dir.path().join("fail-close"), "").unwrap();
    notified(&cfg, &["follower"]);
    crate::cmd::stop(&cfg, "leader", false).unwrap();
    fs::remove_file(dir.path().join("fail-close")).unwrap();
    notified(&cfg, &["leader", "follower", "follower"]);
    assert!(dir.path().join("remote-proj-question-follower-1").exists());
    assert!(!dir.path().join("pending-proj-question-follower-1").exists());
    assert!(!dir.path().join("remote-proj-question-leader-1").exists());
}
