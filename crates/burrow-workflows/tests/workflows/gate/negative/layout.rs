//! Where a tests-only crate keeps its files. The negative knows a test by its
//! path, and such a crate's modules are named for what they test, so a path
//! is all that tells it: all of the crate's code, helpers and module
//! declarations too, is where the negative takes it along to the old code.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use super::Repo;
use crate::harness::{read, repo_file, write};
use crate::test_executables::test_attrs;

fn parsed(file: &Path) -> syn::File {
    syn::parse_file(&read(file)).unwrap_or_else(|e| panic!("{}: {e}", file.display()))
}

/// No program, and no library outside `cargo test`.
fn tests_only(member: &Path) -> bool {
    if member.join("src/main.rs").exists() || member.join("src/bin").exists() {
        return false;
    }
    let lib = member.join("src/lib.rs");
    if !lib.exists() {
        return true;
    }
    let lib = parsed(&lib);
    test_attrs(&lib.attrs)
        || lib
            .items
            .iter()
            .all(|item| matches!(item, syn::Item::Mod(m) if test_attrs(&m.attrs)))
}

/// Each Rust file with code in it under `dir`, by its path from `root`'s
/// parent. A library left empty for Cargo has none to take along.
fn code(root: &Path, dir: &Path, found: &mut BTreeSet<String>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            code(root, &path, found);
        } else if path.extension().is_some_and(|e| e == "rs") && !parsed(&path).items.is_empty() {
            let path = path.strip_prefix(root).unwrap().to_str().unwrap();
            found.insert(format!("crates/{path}"));
        }
    }
}

/// The code of the workspace's tests-only crates, and what of it the negative
/// doesn't take as tests, which the negative itself says of a PR adding it.
fn survey(root: &Path) -> (BTreeSet<String>, Vec<String>) {
    let mut files = BTreeSet::new();
    for entry in fs::read_dir(root).unwrap() {
        let member = entry.unwrap().path();
        if member.join("Cargo.toml").is_file() && tests_only(&member) {
            code(root, &member, &mut files);
        }
    }
    let r = Repo::new();
    let added: Vec<_> = files.iter().map(|f| (f.as_str(), Some("x\n"))).collect();
    let ran = r.run(&r.pr("layout", &added), "", "");
    let tests = ran.list("tests");
    let outside = files
        .iter()
        .filter(|f| !tests.contains(&f.as_str()))
        .cloned()
        .collect();
    (files, outside)
}

/// burrow-workflows had 27 of its 29 files with tests outside the patterns,
/// so a change to them got no negative (#1406).
#[test]
fn a_tests_only_crates_code_is_all_tests_to_the_negative() {
    let (files, outside) = survey(&repo_file("crates"));
    assert!(
        files
            .iter()
            .any(|f| f.starts_with("crates/burrow-workflows/")),
        "burrow-workflows isn't seen as tests only: {files:?}"
    );
    assert!(
        outside.is_empty(),
        "the negative doesn't take these as tests: keep a tests-only crate's code under its tests/\n{}",
        outside.join("\n")
    );
}

/// Each way a crate is tests only, and one that isn't.
#[test]
fn a_tests_only_crates_misplaced_test_is_found() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let test = "#[test]\nfn t() {}\n";
    for (file, text) in [
        ("whole/Cargo.toml", ""),
        ("whole/src/lib.rs", "#![cfg(test)]\nmod gate;\n"),
        ("whole/src/gate.rs", test),
        ("each/Cargo.toml", ""),
        ("each/src/lib.rs", "#[cfg(test)]\nmod lane;\n"),
        ("each/src/lane.rs", test),
        ("empty/Cargo.toml", ""),
        ("empty/src/lib.rs", "//! Empty.\n"),
        ("empty/src/stray.rs", test),
        ("empty/tests/all/main.rs", "mod train;\n"),
        ("empty/tests/all/train.rs", test),
        ("code/Cargo.toml", ""),
        (
            "code/src/lib.rs",
            "pub fn f() {}\n#[cfg(test)]\nmod checks;\n",
        ),
        ("code/src/checks.rs", test),
    ] {
        write(&root.join(file), text);
    }
    let (_, outside) = survey(root);
    assert_eq!(
        outside,
        [
            "crates/each/src/lane.rs",
            "crates/each/src/lib.rs",
            "crates/empty/src/stray.rs",
            "crates/whole/src/gate.rs",
            "crates/whole/src/lib.rs",
        ]
    );
}
