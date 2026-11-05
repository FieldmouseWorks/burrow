//! Which new tests count as having passed on the old code. Reading a test
//! file isn't running it: a formatter, a lint or a secrets scan reads every
//! file. A check says which tests it runs as its `testFiles`, git glob
//! pathspecs from the repo's root, in the base's flake; one that doesn't
//! say runs every test it reads. A new test passed on the old code only
//! where a check that passed runs it, and one no check here runs is for the
//! gate that does to judge: skipped, which counts for nothing.

use super::{Repo, WANTS_2};

/// The PR every case here makes: a code change, and a new version of the
/// test t reads.
fn vacuous(r: &Repo) -> String {
    r.pr(
        "vacuous",
        &[
            ("lib.txt", Some("2\n")),
            ("tests/want.txt", Some("1\n# also\n")),
        ],
    )
}

/// A PR whose new test passes on the old code too doesn't test its change:
/// red, and it says what to do.
#[test]
fn a_test_that_passes_on_the_old_code_is_red() {
    let r = Repo::new();
    let ran = r.run(&vacuous(&r), "", "");
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert_eq!(ran.result(), "pass");
    assert!(
        ran.out.contains("The new tests pass on the old code"),
        "{}",
        ran.out
    );
    assert_eq!(ran.list("checks"), ["t"]);
    assert!(ran.list("unrun").is_empty(), "{}", ran.out);
}

/// The same where t says it runs the test, by a glob.
#[test]
fn a_check_that_says_it_runs_the_test_counts_its_pass() {
    let r = Repo::declaring(r#"{ testFiles = [ "tests/*.txt" ]; }"#);
    let ran = r.run(&vacuous(&r), "", "");
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert_eq!(ran.result(), "pass", "{}", ran.out);
}

/// A check that reads the test but runs none, or not that one, says
/// nothing of it: skipped, green, with the test named as unrun.
#[test]
fn a_check_that_doesnt_run_the_test_says_nothing_of_it() {
    for runs in ["[ ]", r#"[ "tests/other/**" "lib.txt" ]"#] {
        let r = Repo::declaring(&format!("{{ testFiles = {runs}; }}"));
        let ran = r.run(&vacuous(&r), "", "");
        assert_eq!(ran.code, 0, "{runs}: {}", ran.out);
        assert_eq!(ran.result(), "skipped", "{runs}: {}", ran.out);
        assert_eq!(ran.list("unrun"), ["tests/want.txt"], "{runs}");
        assert_eq!(ran.list("checks"), ["t"], "{runs}");
        assert!(
            ran.out
                .contains("no check that passed here runs these new tests"),
            "{runs}: {}",
            ran.out
        );
    }
}

/// A check that runs no test still fails on the old code when the new
/// tests break it (a lint that compiles them, say): that counts.
#[test]
fn a_failure_counts_whatever_the_check_runs() {
    let r = Repo::declaring("{ testFiles = [ ]; }");
    let head = r.pr(
        "real",
        &[("lib.txt", Some("2\n")), ("tests/want.txt", Some("2\n"))],
    );
    let ran = r.run(&head, "", WANTS_2);
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
}

/// A testFiles that isn't a list of paths from the repo's root (a pathspec
/// of git's own magic, a path out of the tree, a string) can't tell.
#[test]
fn a_testfiles_that_isnt_paths_cant_tell() {
    for runs in [
        r#"[ ":(exclude)lib.txt" ]"#,
        r#"[ "../x" ]"#,
        r#"[ "/tests" ]"#,
        r#""tests/**""#,
    ] {
        let r = Repo::declaring(&format!("{{ testFiles = {runs}; }}"));
        let ran = r.run(&vacuous(&r), "", "");
        assert_eq!(ran.code, 1, "{runs}: {}", ran.out);
        assert!(ran.found.is_none(), "{runs}: {}", ran.out);
        assert!(
            ran.out.contains("testFiles isn't a list of paths"),
            "{runs}: {}",
            ran.out
        );
    }
}

/// Test files no check reads aren't run here either: skipped, with nothing
/// built. (They were red, as tests nothing runs; but the negative sees only
/// the flake, and a repo's other gates may run them.)
#[test]
fn tests_no_check_reads_are_unrun() {
    let r = Repo::new();
    let head = r.pr(
        "unread",
        &[("lib.txt", Some("2\n")), ("docs/tests/x.txt", Some("x\n"))],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "skipped", "{}", ran.out);
    assert_eq!(ran.list("unrun"), ["docs/tests/x.txt"]);
    assert!(ran.builds().is_empty(), "{:?}", ran.calls);
}
