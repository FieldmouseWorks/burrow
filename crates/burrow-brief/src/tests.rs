//! The briefs' text, and that no other code holds a copy of any of it. The
//! briefs as main rendered them before the text moved here are kept in
//! tests/golden, which the steward's tests (tests/autoland/briefs.py) hold
//! their callers to.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use super::*;

#[test]
fn a_fix_job_weighs_findings_on_their_merits() {
    // What must be fixed is one text, in what a fix job is told and in
    // what a judge past the limit counts as failing.
    assert!(FINDINGS.contains(&format!("Fix in full each one that is {MUST_FIX}: ")));
    assert!(PAST_LIMIT.contains(&format!("that is {MUST_FIX} counts as failing")));
    assert!(ANSWERED.contains(&format!("that is {MUST_FIX} counts as failing")));
    for said in [
        "Weigh every finding on its merits, the same from the first round on, past the review-round limit too, whatever severity the reviewer gave it. ",
        "with a regression test for each of those cases that fails without the fix, not only the line the thread is on. ",
        "Answer the rest with your reasons and change nothing for them: ",
        "A finding labelled P1 that names no rule on that P1 list, no reason in the PR's description that doesn't hold (settled as below) and no concrete failure scenario counts as P2 here: its label alone doesn't make it one to fix. ",
    ] {
        assert!(FINDINGS.contains(said), "{said}");
    }
    // It's followed by the next sentence the fix job's task gives.
    assert!(FINDINGS.ends_with(". "), "{FINDINGS}");
}

#[test]
fn the_cli_gives_every_text_by_name_for_its_owner() {
    let t = texts("Sam");
    let got: HashSet<&str> = t.as_object().unwrap().keys().map(String::as_str).collect();
    let want: HashSet<&str> = [
        "must_fix",
        "findings",
        "reviewed",
        "past_limit",
        "answered",
        "dep_seen",
        "failing",
        "land_or_owner",
        "reasons",
    ]
    .into();
    assert_eq!(got, want);
    assert_eq!(t["findings"], FINDINGS);
    assert_eq!(t["past_limit"], PAST_LIMIT);
    assert_eq!(t["answered"], ANSWERED);
    let land = t["land_or_owner"].as_str().unwrap();
    assert!(
        land.contains(" Send it to Sam only if it can't be undone "),
        "{land}"
    );
    assert!(land.contains("reserves for Sam, and only if"), "{land}");
}

/// Every stretch of `WINDOW` characters of fixed text the briefs give:
/// each text the CLI gives. A copy of any of it elsewhere has one of them whole.
const WINDOW: usize = 40;

fn policy() -> HashSet<String> {
    // Fill the owner with a mark no text has, so what's left between the
    // marks is only the texts' own words.
    const MARK: &str = "\u{0}";
    let mut texts: Vec<String> = vec![];
    for (_, v) in super::texts(MARK).as_object().unwrap() {
        texts.push(v.as_str().unwrap().to_string());
    }
    let mut windows = HashSet::new();
    for text in texts {
        for piece in text.split(MARK) {
            let chars: Vec<char> = piece.chars().collect();
            for w in chars.windows(WINDOW) {
                windows.insert(w.iter().collect());
            }
        }
    }
    windows
}

/// The code a brief's text could be copied into: burrow's Rust, Python,
/// Nix and shell, but for this crate, what builds leave and the tests,
/// which pin the text rather than give it.
fn code(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if path.is_dir() {
            let skip = ["target", ".git", "burrow-brief", "tests", "result"];
            if !skip.contains(&name.as_str()) && !name.starts_with("result-") {
                code(&path, out);
            }
        } else if ["rs", "py", "nix", "sh"]
            .iter()
            .any(|ext| name.ends_with(&format!(".{ext}")))
            && !name.ends_with("_tests.rs")
            && name != "tests.rs"
            && name != "testing.rs"
        {
            out.push(path);
        }
    }
}

/// Where the text's copies lived before it moved here, from the root of
/// burrow's tree: the crates, and the steward's tasks beside them.
fn roots() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    vec![root.join("crates"), root.join("modules/autoland")]
}

fn copies(roots: &[PathBuf], windows: &HashSet<String>) -> Vec<String> {
    let mut files = vec![];
    for root in roots {
        code(root, &mut files);
    }
    let mut found = vec![];
    for file in files {
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            if let Some(w) = chars
                .windows(WINDOW)
                .map(|w| w.iter().collect::<String>())
                .find(|w| windows.contains(w))
            {
                found.push(format!("{}:{}: {w:?}", file.display(), n + 1));
                break;
            }
        }
    }
    found
}

#[test]
fn no_other_code_holds_a_copy_of_the_briefs_text() {
    let roots = roots();
    for root in &roots {
        assert!(root.is_dir(), "{} isn't there to read", root.display());
    }
    let found = copies(&roots, &policy());
    assert!(
        found.is_empty(),
        "the briefs' text, copied:\n{}",
        found.join("\n")
    );
}

#[test]
fn a_copy_of_the_briefs_text_is_found() {
    // The scan, on a copy as a caller would hold it: a line of a text in
    // Rust, and a piece of a text split across Python's lines.
    let dir = std::env::temp_dir().join(format!("burrow-brief-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(dir.join("tests")).unwrap();
    fs::write(
        dir.join("caller.rs"),
        "let s = \"Weigh every finding on its merits, the same from the first round on\";\n",
    )
    .unwrap();
    fs::write(
        dir.join("caller.py"),
        "X = (\n    \"its reviewers found nothing on that head, \"\n    \"every thread is resolved\"\n)\n",
    )
    .unwrap();
    // A test pinning the text isn't a copy that gives it.
    fs::write(
        dir.join("tests/pin.py"),
        "assert \"Weigh every finding on its merits, the same from the first round on\" in p\n",
    )
    .unwrap();
    fs::write(
        dir.join("clean.py"),
        "print('nothing of the briefs here at all, none')\n",
    )
    .unwrap();
    let found = copies(std::slice::from_ref(&dir), &policy());
    fs::remove_dir_all(&dir).unwrap();
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found.iter().any(|f| f.contains("caller.rs:1:")),
        "{found:?}"
    );
    assert!(
        found.iter().any(|f| f.contains("caller.py:2:")),
        "{found:?}"
    );
}

#[test]
fn a_description_is_quoted_line_by_line_and_none_says_so() {
    assert_eq!(super::quote("  one\n\ntwo  \n"), "> one\n>\n> two");
    assert_eq!(super::quote(" \n "), "(none)");
}
