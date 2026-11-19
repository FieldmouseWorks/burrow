//! A check that never ends on the old code. A test that a wait stops waits
//! forever on the code with that bug, so its check is built once, under
//! the negative's timeout once what it's built from has built, and never a
//! second time: its timing out is the new tests catching the base where
//! the same check passes on the head and on the base (#1212).

use super::{Repo, WANTS_2};
use crate::harness::{read, write};

/// t's derivation on the negative tree alone: the base's code with the
/// head's test.
const NEGATIVE: &str = r#""pair":"1\n/2\n""#;

/// The repo, with t holding its code and its test together, so a case can
/// time out the negative tree's t alone; and the PR, which changes both.
fn paired() -> (Repo, String) {
    let r = Repo::new();
    let flake = read(&r.origin.join("flake.nix"));
    let paired = flake.replace(
        "inherit system want;",
        r#"inherit system want; pair = builtins.readFile ./lib.txt + "/" + want;"#,
    );
    assert_ne!(paired, flake);
    write(&r.origin.join("flake.nix"), &paired);
    r.g.s
        .git(&r.origin, &["commit", "-qam", "t pairs code and test"]);
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    (r, head)
}

/// Timing out on the old code, and passing on the head and the base, is
/// the new tests failing there: built once, with the timeout, and said so.
#[test]
fn a_check_that_never_ends_on_the_old_code_is_caught_where_the_head_passes() {
    let (r, head) = paired();
    let ran = r.run_with(&head, "", &[("BUILD_TIMEOUT_TEXT", NEGATIVE)]);
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
    assert_eq!(ran.list("checks"), ["t"]);
    assert!(
        ran.out.contains("timed out after 30 minutes")
            && ran
                .out
                .contains("passes on the head: the new tests catch the base"),
        "{}",
        ran.out
    );
    // The negative tree's t, the base's and the head's, each built once.
    let mut checks: Vec<_> = ran
        .builds()
        .into_iter()
        .filter(|b| b.contains("-t.drv^*"))
        .collect();
    assert_eq!(checks.len(), 3, "{checks:?}");
    checks.sort();
    checks.dedup();
    assert_eq!(checks.len(), 3, "{checks:?}");
    // Each check with the negative's timeout; what it's built from with
    // none.
    let timeouts = std::fs::read_to_string(r.g.s.join("nix.log.timeout")).unwrap();
    for line in timeouts.lines() {
        let want = if line.contains("-t.drv^*") {
            "1800 "
        } else {
            "0 "
        };
        assert!(line.starts_with(want), "{timeouts}");
    }
}

/// Timing out where the check doesn't pass on the head, or on the base,
/// or in what it's built from, can't tell: nothing's attested and the job
/// fails.
#[test]
fn a_timeout_that_isnt_the_new_tests_own_cant_tell() {
    let (r, head) = paired();
    for (text, says) in [
        (WANTS_2, "the same check on the head, doesn't pass"),
        (r#""name":"t""#, "the same check on the base, fails too"),
        (r#""name":"dep""#, "didn't build"),
    ] {
        let ran = r.run_with(&head, "", &[("BUILD_TIMEOUT_TEXT", text)]);
        assert_eq!(ran.code, 1, "{says}: {}", ran.out);
        assert!(ran.found.is_none(), "{says}: {}", ran.out);
        assert!(ran.out.contains(says), "{says}: {}", ran.out);
    }
}
