//! A failure the negative tree made itself, by taking away a test file the
//! old code names, isn't the new tests' (#1418). It counts only where the
//! removal alone doesn't break the check, or where the check fails with
//! those files put back too.

use super::{Ran, Repo};
use crate::harness::{read, write};

/// t where the removal took its test away, as a module's file is taken
/// from the code that declares it.
const GONE: &str = r#""want":"nothing""#;
/// t with the new test on the old code: failing, or not compiling for
/// want of what the change adds.
const NEW_FAILS: &str = r#""pair":"1\n/2\n""#;
/// t with both spellings of one module, which rustc refuses.
const BOTH: &str = r#""part":"both""#;
/// t with neither.
const NEITHER: &str = r#""part":"neither""#;

/// A PR's files.
type Files<'a> = Vec<(&'a str, Option<&'a str>)>;

/// The repo, with t pairing the code with a test file of its own, so a
/// case can fail the new test alone, and saying which spelling of a module
/// it finds; and with a test nothing names.
fn repo() -> Repo {
    let r = Repo::new();
    edit(
        &r,
        "inherit system want;",
        r#"inherit system want;
          pair = builtins.readFile ./lib.txt + "/" + (if builtins.pathExists ./tests/new.txt then builtins.readFile ./tests/new.txt else "none");
          part = if builtins.pathExists ./tests/part.rs
            then (if builtins.pathExists ./tests/part/mod.rs then "both" else "file")
            else (if builtins.pathExists ./tests/part/mod.rs then "directory" else "neither");"#,
    );
    write(&r.origin.join("tests/part.rs"), "part\n");
    write(&r.origin.join("tests/unnamed.txt"), "unnamed\n");
    r.g.s.git(&r.origin, &["add", "-A"]);
    r.g.s
        .git(&r.origin, &["commit", "-qm", "t pairs code and test"]);
    r
}

fn edit(r: &Repo, from: &str, to: &str) {
    let flake = read(&r.origin.join("flake.nix"));
    let edited = flake.replace(from, to);
    assert_ne!(edited, flake, "{from}");
    write(&r.origin.join("flake.nix"), &edited);
}

/// A code change that moves the test the old code names, as it was.
fn moved() -> Files<'static> {
    vec![
        ("lib.txt", Some("2\n")),
        ("tests/want.txt", None),
        ("tests/moved/want.txt", Some("1\n")),
    ]
}

fn cant_tell(ran: &Ran) {
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(
        ran.out.contains("the tree made that failure"),
        "{}",
        ran.out
    );
}

fn caught(ran: &Ran) {
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert_eq!(ran.result(), "fail", "{}", ran.out);
}

/// The moved test passes on old code and new alike: t fails only for the
/// file taken away, so nothing is signed.
#[test]
fn a_moved_test_the_old_code_names_is_no_catch() {
    let r = repo();
    let ran = r.run(&r.pr("moved", &moved()), "", GONE);
    cant_tell(&ran);
}

/// The same where t never ends without the file, and passes on the head,
/// which reads it where it went.
#[test]
fn a_wait_for_a_file_taken_away_is_no_catch() {
    let r = repo();
    let flake =
        read(&r.origin.join("flake.nix")).replace("./tests/want.txt", "./tests/moved/want.txt");
    let mut files = moved();
    files.push(("flake.nix", Some(flake.as_str())));
    let ran = r.run_with(&r.pr("waits", &files), "", &[("BUILD_TIMEOUT_TEXT", GONE)]);
    cant_tell(&ran);
}

/// A new test that doesn't compile for want of the change still fails
/// there: alone, beside a deletion that breaks nothing, beside a move that
/// does, and beside a module split where the old code still finds it.
#[test]
fn a_test_wanting_what_the_change_adds_is_still_caught() {
    let r = repo();
    let new = ("tests/new.txt", Some("2\n"));
    let alone = vec![("lib.txt", Some("2\n")), new];
    let mut deleting = alone.clone();
    deleting.push(("tests/unnamed.txt", None));
    let mut moving = moved();
    moving.push(new);
    let mut splitting = alone.clone();
    splitting.extend([
        ("tests/part.rs", None),
        ("tests/part/mod.rs", Some("part\n")),
    ]);
    for (name, files) in [
        ("alone", alone),
        ("deleting", deleting),
        ("moving", moving),
        ("splitting", splitting),
    ] {
        let fail = format!("{GONE} {NEW_FAILS} {BOTH} {NEITHER}");
        let ran = r.run_with(
            &r.pr(name, &files),
            "",
            &[("BUILD_FAIL_TEXT", &fail), ("BUILD_EXIT", "101")],
        );
        caught(&ran);
        assert_eq!(ran.list("checks"), ["t"], "{name}");
        // No derivation of t's is built twice.
        let mut checks: Vec<_> = ran
            .builds()
            .into_iter()
            .filter(|b| b.contains("-t.drv^*"))
            .collect();
        let built = checks.len();
        checks.sort();
        checks.dedup();
        assert_eq!(checks.len(), built, "{name}: {}", ran.out);
    }
}

/// A test of a wait beside a move: with the file put back it never ends on
/// the old code, and it passes on the head.
#[test]
fn a_wait_beside_a_move_is_still_caught() {
    let r = repo();
    let flake =
        read(&r.origin.join("flake.nix")).replace("./tests/want.txt", "./tests/moved/want.txt");
    let mut files = moved();
    files.extend([
        ("flake.nix", Some(flake.as_str())),
        ("tests/new.txt", Some("2\n")),
    ]);
    let ran = r.run_with(
        &r.pr("waits", &files),
        "",
        &[
            ("BUILD_FAIL_TEXT", GONE),
            (
                "BUILD_TIMEOUT_TEXT",
                r#""pair":"1\n/2\n","part":"file","system""#,
            ),
        ],
    );
    caught(&ran);
}

/// Putting back makes no failure of its own: a file the PR replaced, at
/// the other spelling of its module, stays away.
#[test]
fn a_replaced_file_isnt_put_back() {
    let r = repo();
    let mut files = moved();
    files.extend([
        ("tests/part.rs", None),
        ("tests/part/mod.rs", Some("part\n")),
    ]);
    let ran = r.run(&r.pr("split", &files), "", &format!("{GONE} {BOTH}"));
    cant_tell(&ran);
}

/// Nor is a file where the PR made a directory, which no tree holds both
/// of.
#[test]
fn a_file_made_a_directory_isnt_put_back() {
    let r = repo();
    let s = &r.g.s;
    let mut files = moved();
    files.push(("tests/unnamed.txt", None));
    r.pr("dir", &files);
    s.git(&r.origin, &["checkout", "-q", "dir"]);
    write(&r.origin.join("tests/unnamed.txt/case"), "case\n");
    s.git(&r.origin, &["add", "-A"]);
    s.git(&r.origin, &["commit", "-qm", "a directory"]);
    let head = s.git(&r.origin, &["rev-parse", "HEAD"]);
    s.git(&r.origin, &["checkout", "-q", "main"]);
    s.git(
        &r.co,
        &[
            "fetch",
            "-q",
            "origin",
            "+refs/heads/*:refs/remotes/origin/*",
        ],
    );
    cant_tell(&r.run(&head, "", GONE));
}

/// An evaluation fails for a file taken away as a build does: a check's
/// own, or the checks' names'. A test that fails with it back is caught.
#[test]
fn an_evaluation_wanting_a_file_taken_away_is_no_catch() {
    let r = repo();
    write(
        &r.origin.join("tests/check.nix"),
        "{ drv }: drv \"fromtests\" (import ./deps.nix)\n",
    );
    write(&r.origin.join("tests/deps.nix"), "[ ]\n");
    write(&r.origin.join("tests/names.nix"), "{ }\n");
    write(&r.origin.join("tests/must.txt"), "must\n");
    edit(
        &r,
        "checks.${system} = {",
        "checks.${system} = import ./tests/names.nix // {",
    );
    edit(
        &r,
        "inherit system want;",
        "inherit system want; must = builtins.readFile ./tests/must.txt;",
    );
    r.g.s.git(&r.origin, &["add", "-A"]);
    r.g.s
        .git(&r.origin, &["commit", "-qm", "tests the flake reads"]);

    let code = ("lib.txt", Some("2\n"));
    for (name, file, at, text) in [
        ("one", "tests/deps.nix", "tests/moved/deps.nix", "[ ]\n"),
        ("names", "tests/names.nix", "tests/moved/names.nix", "{ }\n"),
        ("read", "tests/must.txt", "tests/moved/must.txt", "must\n"),
    ] {
        let head = r.pr(name, &[code, (file, None), (at, Some(text))]);
        cant_tell(&r.run(&head, "", ""));
    }
    // Put back, the test's own expression still throws.
    let head = r.pr(
        "throws",
        &[
            code,
            ("tests/deps.nix", None),
            (
                "tests/check.nix",
                Some("{ drv }: throw \"needs what the change adds\"\n"),
            ),
        ],
    );
    caught(&r.run(&head, "", ""));
    // Put back, it evaluates and the new test fails its build.
    let head = r.pr(
        "builds",
        &[
            code,
            ("tests/must.txt", None),
            ("tests/new.txt", Some("2\n")),
        ],
    );
    caught(&r.run(&head, "", NEW_FAILS));
}
