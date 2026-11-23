//! Which new tests count as having passed on the old code. Reading a test
//! file isn't running it: a formatter, a lint or a secrets scan reads every
//! file. A check says which tests it runs as its `testFiles`, git glob
//! pathspecs from the repo's root, in the base's flake; one that doesn't
//! say runs every test it reads. A new test passed on the old code only
//! where a check that passed runs it, and one no check here runs is for the
//! gate that does to judge: skipped, which counts for nothing.

use super::{Repo, WANTS_2};
use crate::harness::{read, repo_file};

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

/// The same where t says it runs the test, by any of git's globs.
#[test]
fn a_check_that_says_it_runs_the_test_counts_its_pass() {
    for runs in [
        r#""tests/*.txt""#,
        r#""tests/w?nt.txt""#,
        r#""tests/[vw]ant.txt""#,
    ] {
        let r = Repo::declaring(&format!("{{ testFiles = [ {runs} ]; }}"));
        let ran = r.run(&vacuous(&r), "", "");
        assert_eq!(ran.code, 1, "{runs}: {}", ran.out);
        assert_eq!(ran.result(), "pass", "{runs}: {}", ran.out);
    }
}

/// A file's name is compared as git has it, whatever it holds: a test
/// with quotes, a space and a non-ASCII letter in its name, run by a
/// check that says so or read by one that doesn't, isn't unrun.
#[test]
fn a_test_is_known_by_its_whole_name() {
    for t_extra in ["{ }", r#"{ testFiles = [ "tests/**" ]; }"#] {
        let r = Repo::declaring(t_extra);
        let head = r.pr(
            "named",
            &[
                ("lib.txt", Some("2\n")),
                ("tests/wänt \"q\".txt", Some("x\n")),
            ],
        );
        let ran = r.run(&head, "", "");
        assert_eq!(ran.result(), "pass", "{t_extra}: {}", ran.out);
        assert!(ran.list("unrun").is_empty(), "{t_extra}: {}", ran.out);
    }
}

/// A check that doesn't say runs only the new tests it reads: t reads
/// tests/, so a new test elsewhere is unrun beside one t runs.
#[test]
fn a_check_that_doesnt_say_runs_only_what_it_reads() {
    let r = Repo::new();
    let head = r.pr(
        "mixed",
        &[
            ("lib.txt", Some("2\n")),
            ("tests/want.txt", Some("1\n# also\n")),
            ("docs/tests/x.txt", Some("x\n")),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "skipped", "{}", ran.out);
    assert_eq!(ran.list("unrun"), ["docs/tests/x.txt"], "{}", ran.out);
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
        // Flagged on the PR, never quietly green.
        assert!(
            ran.out
                .contains("::warning title=New tests the flake doesn't run::"),
            "{runs}: {}",
            ran.out
        );
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

/// A test the check can't evaluate without, on its own expression, counts
/// as read: the new tests/check.nix imports the new tests/helper.nix.
#[test]
fn a_test_the_check_cant_evaluate_without_counts_as_read() {
    let r = Repo::declaring("{ testFiles = [ ]; }");
    let head = r.pr(
        "helper",
        &[
            ("lib.txt", Some("2\n")),
            (
                "tests/check.nix",
                Some("{ drv }: import ./helper.nix { inherit drv; }\n"),
            ),
            (
                "tests/helper.nix",
                Some("{ drv }: drv \"fromtests\" [ ./helper.nix ]\n"),
            ),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.result(), "pass", "{}", ran.out);
    assert!(ran.list("unrun").is_empty(), "{}", ran.out);
    assert!(
        ran.out
            .contains("doesn't evaluate without tests/helper.nix"),
        "{}",
        ran.out
    );
}

/// A probe that fails for anything but its own expression (killed, here)
/// says nothing of what the check reads: it can't tell, rather than count
/// the unread test as read.
#[test]
fn a_probe_killed_cant_tell() {
    let r = Repo::new();
    let head = r.pr(
        "mixed",
        &[
            ("lib.txt", Some("2\n")),
            ("tests/want.txt", Some("1\n# also\n")),
            ("docs/tests/x.txt", Some("x\n")),
        ],
    );
    let ran = r.run_with(&head, "", &[("EVAL_KILL", "/one/")]);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(ran.out.contains("ended with 137"), "{}", ran.out);
}

/// A test another changed file brings in counts as read: fromtests reads
/// tests/new.txt only once the new tests/check.nix does, so neither alone
/// on the old code would show it, but taking either out of the negative
/// tree changes what fromtests is.
#[test]
fn a_test_brought_in_by_another_counts_as_read() {
    let r = Repo::declaring("{ testFiles = [ ]; }");
    let head = r.pr(
        "companion",
        &[
            ("lib.txt", Some("2\n")),
            ("tests/new.txt", Some("x\n")),
            (
                "tests/check.nix",
                Some(
                    "{ drv }: drv \"fromtests\" (if builtins.pathExists ./new.txt then [ ./new.txt ] else [ ])\n",
                ),
            ),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert_eq!(ran.result(), "pass", "{}", ran.out);
    assert!(ran.list("unrun").is_empty(), "{}", ran.out);
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
        r#""null""#,
        r#"[ "tests/\n" ]"#,
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

/// lib/project.nix's rust-test reads the whole workspace but runs only its
/// packages' tests, and says so (lib/rust-test-files.nix): t here is that
/// check over a workspace in tests/ with one package, a, and fixtures
/// beside it. A test in a crate the base doesn't have is unrun, though t
/// reads it (#699); one that doesn't say would count it as passing.
#[test]
fn a_test_of_a_crate_the_base_lacks_is_unrun() {
    let declared = r#"{ testFiles = import ./rust-test-files.nix { root = ./tests; prefix = "tests"; packages = [ "a" ]; include = [ ]; }; }"#;
    for (t_extra, result) in [(declared, "skipped"), ("{ }", "pass")] {
        let r = workspace(t_extra);
        let head = r.pr(
            "new-crate",
            &[
                ("lib.txt", Some("2\n")),
                ("tests/b/Cargo.toml", Some("[package]\nname = \"b\"\n")),
                ("tests/b/src/pass_tests.rs", Some("// b\n")),
            ],
        );
        let ran = r.run(&head, "", "");
        assert_eq!(ran.result(), result, "{t_extra}: {}", ran.out);
        if result == "skipped" {
            assert_eq!(ran.code, 0, "{}", ran.out);
            assert_eq!(
                ran.list("unrun"),
                ["tests/b/Cargo.toml", "tests/b/src/pass_tests.rs"],
                "{}",
                ran.out
            );
        }
    }
}

/// The same declaration still runs the tests of the packages the base
/// has, a first tests/ directory included, and the fixtures beside them:
/// new ones that pass on the old code are red.
#[test]
fn a_test_of_a_crate_the_base_has_still_runs() {
    let r = workspace(
        r#"{ testFiles = import ./rust-test-files.nix { root = ./tests; prefix = "tests"; packages = [ "a" ]; include = [ ]; }; }"#,
    );
    let head = r.pr(
        "member",
        &[
            ("lib.txt", Some("2\n")),
            ("tests/a/tests/more.rs", Some("// more\n")),
            ("tests/a/src/more_tests.rs", Some("// more\n")),
            ("tests/fixtures/new.txt", Some("new\n")),
        ],
    );
    let ran = r.run(&head, "", "");
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert_eq!(ran.result(), "pass", "{}", ran.out);
    assert!(ran.list("unrun").is_empty(), "{}", ran.out);
}

/// t over a workspace in tests/: package a and fixtures beside it, and
/// lib/rust-test-files.nix where t can import it.
fn workspace(t_extra: &str) -> Repo {
    Repo::holding(
        t_extra,
        &[
            (
                "rust-test-files.nix",
                &read(&repo_file("lib/rust-test-files.nix")),
            ),
            ("tests/Cargo.toml", "[workspace]\nmembers = [ \"a\" ]\n"),
            ("tests/a/Cargo.toml", "[package]\nname = \"a\"\n"),
            ("tests/a/src/lib.rs", "// a\n"),
            ("tests/fixtures/data.txt", "data\n"),
        ],
    )
}
