//! Every program in the workspace reaches GitHub's API one way: through this
//! crate, which asks again when GitHub doesn't answer (a 502, a reset). A
//! program with its own `gh api` treats a blip as GitHub's no, and in CI
//! that paints a PR red for nothing (#477). gh-token is gh itself, as the
//! machine runs it, so it's the one other place `gh api` is named.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

/// Crates that still run `gh api` themselves, each to come through this
/// crate (#491). The list only shrinks: one that no longer does fails here
/// until it's taken off.
const STILL: [&str; 2] = ["burrow-proj", "burrow-renovate"];

/// Its code as built, less its tests: a file of tests, or what follows a
/// file's `#[cfg(test)]`.
fn code(dir: &Path, out: &mut String) {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if p.is_dir() {
            if name != "tests" {
                code(&p, out);
            }
        } else if name.ends_with(".rs") && !matches!(name.as_str(), "tests.rs" | "testing.rs") {
            let text = fs::read_to_string(&p).unwrap();
            out.push_str(text.split("#[cfg(test)]").next().unwrap());
        }
    }
}

/// The crates whose code names `gh`'s `api` command.
fn strays(crates: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for e in fs::read_dir(crates).unwrap() {
        let dir = e.unwrap().path();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        if !dir.join("src").is_dir() || matches!(name.as_str(), "burrow-github" | "gh-token") {
            continue;
        }
        let mut text = String::new();
        code(&dir.join("src"), &mut text);
        if text.contains("\"api\"") {
            found.insert(name);
        }
    }
    found
}

#[test]
fn one_way_to_github() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let still: BTreeSet<String> = STILL.iter().map(|s| s.to_string()).collect();
    assert_eq!(
        strays(crates),
        still,
        "a crate runs `gh api` itself: reach GitHub through burrow-github \
         (call, call_repeating, query), or take a crate that no longer does off STILL"
    );
}

// What it finds: `gh api` in code, not in a crate's tests.
#[test]
fn finds_a_stray() {
    let root = tempfile::tempdir().unwrap();
    let mk = |rel: &str, text: &str| {
        let p = root.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    mk(
        "stray/src/world.rs",
        "Command::new(\"gh\").args([\"api\", path])",
    );
    mk(
        "tested/src/lib.rs",
        "fn f() {}\n#[cfg(test)]\nmod t { const A: &str = \"api\"; }",
    );
    mk("tested/src/x/tests.rs", "\"api\"");
    mk("tested/src/tests/a.rs", "\"api\"");
    mk("burrow-github/src/lib.rs", "\"api\"");
    mk("gh-token/src/command.rs", "\"api\"");
    assert_eq!(strays(root.path()), BTreeSet::from(["stray".to_string()]));
}
