//! What vulnix says a system has, through its stand-in (testing_tests.rs). The
//! VM tests themselves are a run's, in run_tests.rs.

use std::fs;

use burrow_github::fake::Fake;

use super::*;
use crate::run::once;
use crate::testing::{Machine, sha};

#[test]
fn a_systems_serious_vulnerabilities_are_what_vulnix_scores_seven_and_up() {
    let m = Machine::running_main(&[("nixpkgs", &sha('a'))]);
    let a = m.running();
    let none = vulns(&m.cfg, &a).unwrap();
    assert!(none.is_empty());
    fs::write(
        m.path("vulnix-db"),
        format!(
            "{a} CVE-3 openssl 9.8\n{a} CVE-1 openssl 7.0\n{a} CVE-2 zlib 6.9\n{b} CVE-4 curl 10\n{a} CVE-1 openssl 7.0\n",
            a = sha('a'),
            b = sha('b')
        ),
    )
    .unwrap();
    let found: Vec<String> = vulns(&m.cfg, &a).unwrap().into_iter().collect();
    assert_eq!(found, ["CVE-1 openssl", "CVE-3 openssl"]);
    assert_eq!(m.did("calls"), format!("vulnix {a}\n").repeat(2));
}

#[test]
fn a_scan_that_didnt_happen_is_none_never_nothing_found() {
    let m = Machine::running_main(&[("nixpkgs", &sha('a'))]);
    let a = m.running();
    // vulnix failing, or saying what isn't its report: no list, an entry
    // without what it's affected by, a CVE that isn't a name, or nothing.
    fs::write(m.path("vulnix-down"), "").unwrap();
    assert_eq!(vulns(&m.cfg, &a), None);
    fs::remove_file(m.path("vulnix-down")).unwrap();
    for said in [
        r#"{"pname": "openssl"}"#,
        r#"[{"pname": "openssl"}]"#,
        r#"[{"pname": "openssl", "affected_by": [7]}]"#,
        "",
        "[",
    ] {
        fs::write(m.path("vulnix-says"), said).unwrap();
        assert_eq!(vulns(&m.cfg, &a), None, "{said:?}");
    }
    // A score it doesn't give is no score.
    fs::write(
        m.path("vulnix-says"),
        r#"[{"pname": "openssl", "affected_by": ["CVE-1"]}, {"pname": "zlib", "affected_by": ["CVE-2"], "cvssv3_basescore": {"CVE-2": "high"}}]"#,
    )
    .unwrap();
    assert_eq!(vulns(&m.cfg, &a), Some(Default::default()));
}

#[test]
fn a_daemon_disconnect_in_a_vm_test_is_not_classified_as_flaky() {
    let m = Machine::running_main(&[("nixpkgs", &sha('a')), ("burrow", &sha('b'))]);
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.cfg.state.join("clean-vm"), "2\n").unwrap();
    fs::write(m.home("check-disconnects"), "").unwrap();

    assert_eq!(once(&m.cfg, &mut Fake::default(), false), 1);
    let status = m.status();
    assert_eq!(status["result"], "failed");
    assert_eq!(status["message"], "nix-daemon disconnected");
    // Only the one failed check ran; no flaky retry, notice, job, or memo entry.
    assert_eq!(m.kept("nix.log").matches("#checks.").count(), 1);
    assert!(!m.cfg.state.join("logs/flaky-vm.log").exists());
    assert!(!m.cfg.state.join("check-results").exists());
    assert_eq!(m.recorded("clean-vm").as_deref(), Some("2"));
    assert!(!m.did("notices").contains("flaky-vm"));
    assert!(!m.kept("proj.log").contains("flaky"));

    // Next run, once nix-daemon is back, passes and deploys cleanly.
    assert_eq!(once(&m.cfg, &mut Fake::default(), false), 0);
    assert_eq!(m.status()["result"], "ok");
    assert_eq!(m.recorded("clean-vm").as_deref(), Some("3"));
}

#[test]
fn a_daemon_disconnect_on_vm_retry_or_main_baseline_does_not_record_failure() {
    // Disconnect on the second (flaky-retry) attempt leaves no memo or broken-test job.
    let m = Machine::running_main(&[("nixpkgs", &sha('a')), ("burrow", &sha('b'))]);
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.home("check-flaky"), "").unwrap();
    fs::write(
        m.home("after-check"),
        format!("touch '{}'\n", m.home("check-disconnects").display()),
    )
    .unwrap();
    assert_eq!(once(&m.cfg, &mut Fake::default(), false), 1);
    assert_eq!(m.status()["message"], "nix-daemon disconnected");
    assert_eq!(m.kept("nix.log").matches("#checks.").count(), 2);
    assert!(!m.cfg.state.join("check-results").exists());
    assert!(m.kept("proj.log").is_empty());

    // Disconnect on the baseline check against main leaves main unpoisoned and no fix-main job.
    let m = Machine::running_main(&[("nixpkgs", &sha('a')), ("burrow", &sha('b'))]);
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.home("check-breaks"), sha('c')).unwrap();
    // First attempt fails (check-breaks) and arms second attempt's after-check,
    // which arms check-disconnects for the third check (on main).
    fs::write(
        m.home("after-check"),
        format!(
            "printf \"touch '{}'\\n\" >'{}'\n",
            m.home("check-disconnects").display(),
            m.home("after-check").display()
        ),
    )
    .unwrap();
    assert_eq!(once(&m.cfg, &mut Fake::default(), false), 1);
    assert_eq!(m.status()["message"], "nix-daemon disconnected");
    assert_eq!(m.kept("nix.log").matches("#checks.").count(), 3);
    assert!(m.kept("proj.log").is_empty());
    // Head's genuine double failure was recorded, but main's disconnected check was not.
    let results = fs::read_to_string(m.cfg.state.join("check-results")).unwrap();
    assert_eq!(results.lines().count(), 1, "{results}");
}
