//! A failure the negative tree made itself, by taking away a test file the
//! old code names, isn't the new tests' (#1418). It counts only where the
//! removal alone doesn't break the check, or where the check fails with
//! those files put back too, on paths a control shows it passes on.

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
/// t with the new spelling alone, as `#[path]` to the old one can't read.
const DIRECTORY: &str = r#""part":"directory""#;
/// t with a failing test in the file that replaced the module's.
const SPLIT_FAILS: &str = r#""body":"2\n""#;

/// A PR's files.
type Files<'a> = Vec<(&'a str, Option<&'a str>)>;

/// The repo, with t pairing the code with a test file of its own, so a
/// case can fail the new test alone, and saying which spelling of a module
/// it finds and what the new one holds; and with a test nothing names.
fn repo() -> Repo {
    let r = Repo::new();
    edit(
        &r,
        "inherit system want;",
        r#"inherit system want;
          pair = builtins.readFile ./lib.txt + "/" + (if builtins.pathExists ./tests/new.txt then builtins.readFile ./tests/new.txt else "none");
          body = if builtins.pathExists ./tests/part/mod.rs then builtins.readFile ./tests/part/mod.rs else "none";
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
/// file taken away, so nothing is signed. The same for a test changed as
/// it moved: the old code never runs it, so it can't be seen to fail there.
#[test]
fn a_moved_test_the_old_code_names_is_no_catch() {
    let r = repo();
    for (name, text) in [("moved", "1\n"), ("moved-changed", "2\n")] {
        let mut files = moved();
        files.pop();
        files.push(("tests/moved/want.txt", Some(text)));
        cant_tell(&r.run(&r.pr(name, &files), "", GONE));
    }
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

/// Putting back makes no failure of its own: a file the PR replaced, where
/// the old code finds the other spelling of its module, stays away.
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

/// A replaced file's absence is no catch where the old code names that
/// very path (`#[path]`): not alone, beside another move, nor where the two
/// spellings can't stand together.
#[test]
fn a_replaced_file_the_old_code_names_is_no_catch() {
    let r = repo();
    let split = vec![
        ("lib.txt", Some("2\n")),
        ("tests/part.rs", None),
        ("tests/part/mod.rs", Some("part\n")),
    ];
    let mut beside_a_move = split.clone();
    beside_a_move.extend(moved().into_iter().skip(1));
    let named = format!("{DIRECTORY} {NEITHER}");
    for (name, files, fail) in [
        ("path", split.clone(), named.clone()),
        ("path-clash", split, format!("{named} {BOTH}")),
        ("path-move", beside_a_move, format!("{named} {GONE}")),
    ] {
        let ran = r.run(&r.pr(name, &files), "", &fail);
        assert_eq!(ran.code, 1, "{name}: {}", ran.out);
        assert!(ran.found.is_none(), "{name}: {}", ran.out);
    }
}

/// A file made a directory breaks the old code's read of it, whether it
/// must be there or is only read where it is: no catch either, for a check
/// or for the checks' names.
#[test]
fn a_directory_where_the_old_code_reads_a_file_is_no_catch() {
    for (file, text, from, to) in [
        (
            "tests/data",
            "data\n",
            "inherit system want;",
            "inherit system want; data = builtins.readFile ./tests/data;",
        ),
        (
            "tests/data",
            "data\n",
            "inherit system want;",
            r#"inherit system want; data = if builtins.pathExists ./tests/data then builtins.readFile ./tests/data else "none";"#,
        ),
        (
            "tests/names",
            "{ }\n",
            "checks.${system} = {",
            "checks.${system} = import ./tests/names // {",
        ),
    ] {
        let r = repo();
        edit(&r, from, to);
        write(&r.origin.join(file), text);
        r.g.s.git(&r.origin, &["add", "-A"]);
        r.g.s.git(&r.origin, &["commit", "-qm", "a file it reads"]);
        let case = format!("{file}/case");
        let head = r.pr(
            "dir",
            &[
                ("lib.txt", Some("2\n")),
                (file, None),
                (case.as_str(), Some(text)),
            ],
        );
        let ran = r.run(&head, "", "");
        assert_eq!(ran.code, 1, "{to}: {}", ran.out);
        assert!(ran.found.is_none(), "{to}: {}", ran.out);
    }
}

/// A test that fails on the old code is still caught beside a replaced
/// file: in a file that stayed, where the old one can go back, and in the
/// replacement, where the old code finds that.
#[test]
fn a_test_beside_a_replaced_file_is_still_caught() {
    let r = repo();
    let code = ("lib.txt", Some("2\n"));
    let new = ("tests/new.txt", Some("2\n"));
    let gone = ("tests/part.rs", None);
    let same = ("tests/part/mod.rs", Some("part\n"));
    let changed = ("tests/part/mod.rs", Some("2\n"));
    let path = vec![code, new, gone, same];
    let split = vec![code, gone, changed];
    let mut split_move = moved();
    split_move.extend([gone, changed]);
    for (name, files, fail) in [
        ("path", path, format!("{DIRECTORY} {NEITHER} {NEW_FAILS}")),
        ("split", split, format!("{BOTH} {NEITHER} {SPLIT_FAILS}")),
        (
            "split-move",
            split_move,
            format!("{GONE} {BOTH} {NEITHER} {SPLIT_FAILS}"),
        ),
    ] {
        let ran = r.run(&r.pr(name, &files), "", &fail);
        assert_eq!(ran.code, 0, "{name}: {}", ran.out);
        assert_eq!(ran.result(), "fail", "{name}: {}", ran.out);
    }
}

/// Where a move breaks the checks' names, each check is judged with the
/// file put back: a failing test there is caught, one that passes signs
/// nothing, and one that isn't built here isn't built.
#[test]
fn a_move_that_breaks_the_names_is_judged_check_by_check() {
    let r = repo();
    edit(
        &r,
        "checks.${system} = {",
        "checks.${system} = import ./tests/names.nix // {",
    );
    write(&r.origin.join("tests/names.nix"), "{ }\n");
    r.g.s.git(&r.origin, &["add", "-A"]);
    r.g.s
        .git(&r.origin, &["commit", "-qm", "names a test gives"]);
    let files = |text| {
        [
            ("lib.txt", Some("2\n")),
            ("tests/names.nix", None),
            ("tests/moved/names.nix", Some("{ }\n")),
            ("tests/new.txt", Some(text)),
        ]
    };
    let head = r.pr("names-moved-catch", &files("2\n"));
    caught(&r.run(&head, "", NEW_FAILS));

    let ran = r.run(&head, "t", NEW_FAILS);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(
        !ran.builds().iter().any(|b| b.contains("-t.drv^*")),
        "{}",
        ran.out
    );

    let head = r.pr("names-moved", &files("1\n"));
    let ran = r.run(&head, "", NEW_FAILS);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
}

/// The repo, with checks of its own from `tests/checks`, so a test file
/// can add one the base hasn't: no control on the base can pass it.
fn checks_from_tests() -> Repo {
    let r = repo();
    edit(
        &r,
        "checks.${system} = {",
        r#"checks.${system} = builtins.listToAttrs (map (f: {
          name = builtins.replaceStrings [ ".nix" ] [ "" ] f;
          value = import (./tests/checks + "/${f}") { inherit system; };
        }) (builtins.attrNames (builtins.readDir ./tests/checks))) // {"#,
    );
    write(
        &r.origin.join("tests/checks/old.nix"),
        r#"{ system }: derivation { name = "old"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }
"#,
    );
    r.g.s.git(&r.origin, &["add", "-A"]);
    r.g.s
        .git(&r.origin, &["commit", "-qm", "checks the tests give"]);
    r
}

/// A check the new tests add, holding the code and, as `extra`, more.
fn new_check(extra: &str) -> String {
    format!(
        r#"{{ system }}: derivation {{ name = "new"; inherit system; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; newlib = builtins.readFile ../../lib.txt; {extra} }}
"#
    )
}

/// The new check on the old code.
const NEW_CHECK_FAILS: &str = r#""newlib":"1\n""#;

/// A check only the new tests define fails on the old code for its own
/// sake beside a replaced test file it never reads, and beside a file made
/// a directory, and beside a directory made a file: caught, as it is beside
/// a plain deletion (#1434).
#[test]
fn a_new_check_beside_a_replaced_file_it_doesnt_read_is_caught() {
    let r = checks_from_tests();
    write(&r.origin.join("tests/d/x"), "x\n");
    r.g.s.git(&r.origin, &["add", "-A"]);
    r.g.s.git(&r.origin, &["commit", "-qm", "a test directory"]);
    let check = new_check("");
    let base = vec![
        ("lib.txt", Some("2\n")),
        ("tests/checks/new.nix", Some(check.as_str())),
    ];
    let mut split = base.clone();
    split.extend([
        ("tests/part.rs", None),
        ("tests/part/mod.rs", Some("part\n")),
    ]);
    let mut directory = base.clone();
    directory.extend([
        ("tests/unnamed.txt", None),
        ("tests/unnamed.txt/case", Some("unnamed\n")),
    ]);
    let mut file = base.clone();
    file.extend([("tests/d/x", None), ("tests/d", Some("d\n"))]);
    let mut deleting = base;
    deleting.push(("tests/unnamed.txt", None));
    for (name, files) in [
        ("new-split", split),
        ("new-directory", directory),
        ("new-file", file),
        ("new-deleting", deleting),
    ] {
        let ran = r.run(&r.pr(name, &files), "", NEW_CHECK_FAILS);
        assert_eq!(ran.code, 0, "{name}: {}", ran.out);
        assert_eq!(ran.result(), "fail", "{name}: {}", ran.out);
    }
}

/// One that reads the replaced file has no control to show its failure
/// is its own and not the file's absence: it can't tell.
#[test]
fn a_new_check_reading_a_replaced_file_cant_tell() {
    let r = checks_from_tests();
    let check =
        new_check(r#"newpart = if builtins.pathExists ../part.rs then "file" else "gone";"#);
    let files = [
        ("lib.txt", Some("2\n")),
        ("tests/checks/new.nix", Some(check.as_str())),
        ("tests/part.rs", None),
        ("tests/part/mod.rs", Some("part\n")),
    ];
    let ran = r.run(&r.pr("new-reads", &files), "", NEW_CHECK_FAILS);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(ran.out.contains("so it reads them"), "{}", ran.out);
}

/// A test directory the PR makes a file of its name (#1426), beside a new
/// test that fails on the old code.
fn directory_made_a_file() -> (Repo, Files<'static>) {
    let r = repo();
    write(&r.origin.join("tests/d/x"), "x\n");
    r.g.s.git(&r.origin, &["add", "-A"]);
    r.g.s.git(&r.origin, &["commit", "-qm", "a test directory"]);
    let files = vec![
        ("lib.txt", Some("2\n")),
        ("tests/new.txt", Some("2\n")),
        ("tests/d/x", None),
        ("tests/d", Some("d\n")),
    ];
    (r, files)
}

/// The negative tree takes the directory's files away before it adds the
/// file, so the test is caught.
#[test]
fn a_directory_made_a_file_is_still_caught() {
    let (r, files) = directory_made_a_file();
    caught(&r.run(&r.pr("file", &files), "", NEW_FAILS));
}

/// Where the old code reads a file in that directory, no tree holds it and
/// the file both, so it can't tell.
#[test]
fn a_file_in_a_directory_made_a_file_isnt_put_back() {
    let (r, files) = directory_made_a_file();
    edit(
        &r,
        "inherit system want;",
        r#"inherit system want; x = if builtins.pathExists ./tests/d/x then "x" else "gone";"#,
    );
    r.g.s.git(&r.origin, &["add", "-A"]);
    r.g.s.git(
        &r.origin,
        &["commit", "-qm", "t reads the directory's file"],
    );
    let ran = r.run(&r.pr("file-read", &files), "", r#""x":"gone""#);
    assert_eq!(ran.code, 1, "{}", ran.out);
    assert!(ran.found.is_none(), "{}", ran.out);
    assert!(
        ran.out
            .contains("nor with it beside what replaced it: a failure there can be the tree's"),
        "{}",
        ran.out
    );
}
