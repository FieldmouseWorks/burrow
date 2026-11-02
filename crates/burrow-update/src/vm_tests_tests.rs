//! What vulnix says a system has, through its stand-in (testing.rs). The
//! VM tests themselves are a run's, in run_tests.rs.

use std::fs;

use super::*;
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
