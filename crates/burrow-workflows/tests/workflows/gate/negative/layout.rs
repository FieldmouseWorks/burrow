//! Where the code only `cargo test` compiles is kept. The negative knows a
//! test by its path, so a path is all that tells it: each member's test-only
//! modules, their helpers too, and all of a tests-only crate, declarations
//! included, are where the negative takes them along to the old code.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use super::Repo;
use crate::harness::{read, repo_file, write};
use crate::test_executables::{test_attrs, test_only};

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
            found.insert(named(root, &path));
        }
    }
}

fn named(root: &Path, file: &Path) -> String {
    format!("crates/{}", file.strip_prefix(root).unwrap().display())
}

/// The code only `cargo test` compiles, and what of it the negative doesn't
/// take as tests, which the negative itself says of a PR adding it.
fn survey(root: &Path) -> (BTreeSet<String>, Vec<String>) {
    let root = &fs::canonicalize(root).unwrap();
    let mut files = BTreeSet::new();
    for entry in fs::read_dir(root).unwrap() {
        let member = entry.unwrap().path();
        if member.join("Cargo.toml").is_file() && tests_only(&member) {
            code(root, &member, &mut files);
        }
    }
    for file in test_only(root) {
        if !parsed(&file).items.is_empty() {
            files.insert(named(root, &file));
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
/// so a change to them got no negative (#1406), and crates with code had
/// test modules and helpers named for what they test (#1407).
#[test]
fn test_only_code_is_all_tests_to_the_negative() {
    let (files, outside) = survey(&repo_file("crates"));
    assert!(
        files
            .iter()
            .any(|f| f.starts_with("crates/burrow-workflows/")),
        "burrow-workflows isn't seen as tests only: {files:?}"
    );
    assert!(
        outside.is_empty(),
        "the negative doesn't take these as tests: keep a tests-only crate's code under its tests/, and name a test-only module's file *_tests.rs\n{}",
        outside.join("\n")
    );
}

/// Each way code is test only, and those it only looks it.
#[test]
fn misplaced_test_only_code_is_found() {
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
            "pub fn f() {}
            #[cfg(test)] mod checks;
            #[cfg(test)] #[path = \"world.rs\"] mod renamed;
            #[cfg(test)] #[path = \"named_tests.rs\"] mod named;
            #[cfg(all(test, unix))] mod unix;
            #[cfg(test)] mod inline { mod within; }
            #[cfg(test)] mod parent_tests;
            mod whole;
            #[cfg(any(test, feature = \"fake\"))] pub mod fake;
            mod shared;
            #[cfg(test)] #[path = \"../src/shared.rs\"] mod again;\n",
        ),
        ("code/src/checks.rs", test),
        ("code/src/world.rs", "pub fn helper() {}\n"),
        ("code/src/named_tests.rs", test),
        ("code/src/unix.rs", test),
        ("code/src/inline/within.rs", test),
        ("code/src/parent_tests.rs", "mod child;\n"),
        ("code/src/parent_tests/child.rs", test),
        ("code/src/whole.rs", "#![cfg(test)]\nfn helper() {}\n"),
        ("code/src/fake.rs", "pub fn f() {}\n"),
        ("code/src/shared.rs", "pub fn f() {}\n"),
        (
            "code/src/bin/tool/main.rs",
            "fn main() {}\n#[cfg(test)] mod stand_in;\n",
        ),
        ("code/src/bin/tool/stand_in.rs", test),
        ("code/tests/it.rs", test),
    ] {
        write(&root.join(file), text);
    }
    let (_, outside) = survey(root);
    assert_eq!(
        outside,
        [
            "crates/code/src/bin/tool/stand_in.rs",
            "crates/code/src/checks.rs",
            "crates/code/src/inline/within.rs",
            "crates/code/src/parent_tests/child.rs",
            "crates/code/src/unix.rs",
            "crates/code/src/whole.rs",
            "crates/code/src/world.rs",
            "crates/each/src/lane.rs",
            "crates/each/src/lib.rs",
            "crates/empty/src/stray.rs",
            "crates/whole/src/gate.rs",
            "crates/whole/src/lib.rs",
        ]
    );
}
