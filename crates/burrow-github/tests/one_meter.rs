//! What a GitHub limit looks like is burrow-github's to say (quota.rs):
//! no other program reads one out of an error's text, a limit's headers or
//! `rate_limit`. The one exception is gh-token's response observer
//! (quota.rs, relay.rs), which records what the gh door sees into the
//! installations' meters. A program that needs to know asks the meter
//! (quota::Meters) or the error's hold (quota::Hold).

use std::fs;
use std::path::{Path, PathBuf};

/// The files allowed to read a limit themselves, from crates/.
const OWNERS: &[&str] = &[
    "burrow-github/",
    "gh-token/src/quota.rs",
    "gh-token/src/relay.rs",
];

/// What reading a limit looks like in a line of code, lower case.
fn reads_a_limit(line: &str) -> Option<&'static str> {
    let line = line.to_lowercase();
    let compares = [
        ".contains(",
        ".find(",
        "starts_with(",
        "ends_with(",
        "==",
        "!=",
        "matches!",
    ]
    .iter()
    .any(|op| line.contains(op));
    let quoted = |text: &str| {
        line.split('"')
            .skip(1)
            .step_by(2)
            .any(|literal| literal.contains(text))
    };
    if compares && quoted("rate limit") {
        Some("matches rate-limit text")
    } else if line.contains("\"rate_limit\"") || line.contains("/rate_limit\"") {
        Some("asks GitHub's rate_limit")
    } else if quoted("x-ratelimit-") || line.contains("\"retry-after\"") {
        Some("reads a limit's headers")
    } else {
        None
    }
}

/// Every .rs file under DIR, but tests/ directories.
fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if !matches!(
                path.file_name().and_then(|n| n.to_str()),
                Some("tests" | "target")
            ) {
                sources(&path, found);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

/// Each line of code outside OWNERS that reads a limit: `FILE:LINE: what`.
/// A file's tests (from a `#[cfg(test)]` at the start of a line on, or a
/// file whose name says it's tests) script GitHub's answers, limits and
/// all, and are left out.
fn findings(crates: &Path) -> Vec<String> {
    let mut files = Vec::new();
    sources(crates, &mut files);
    files.sort();
    let mut out = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(crates)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let name = file.file_name().unwrap().to_string_lossy();
        if OWNERS.iter().any(|o| rel.starts_with(o))
            || name.contains("tests")
            || name == "testing.rs"
            || name.ends_with("_world.rs")
        {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for (n, line) in text.lines().enumerate() {
            if line.starts_with("#[cfg(test)]") {
                break;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            if let Some(what) = reads_a_limit(line) {
                out.push(format!("{rel}:{}: {what}: {}", n + 1, line.trim()));
            }
        }
    }
    out
}

#[test]
fn only_burrow_github_and_the_gh_doors_observer_read_a_limit() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let found = findings(&crates);
    assert!(
        found.is_empty(),
        "read a limit through burrow_github::quota (Meters, Hold) instead:\n{}",
        found.join("\n")
    );
}

#[test]
fn what_reading_a_limit_looks_like() {
    for line in [
        r#"if e.why.to_lowercase().contains("rate limit") {"#,
        r#"let r = call(api, "GET", "rate_limit", None, None);"#,
        r#".args(["api", "/rate_limit"])"#,
        r#"header(&r, "x-ratelimit-remaining") == Some("0")"#,
        r#"|| header(&r, "retry-after").is_some()"#,
    ] {
        assert!(reads_a_limit(line).is_some(), "{line}");
    }
    for line in [
        r#"say(&format!("{what}: booked again, when GitHub's rate limit lifts"));"#,
        r#"if record["err"]["message"] != "rate-limit-exceeded" {"#,
        r#"&v["rate_limit_info"],"#,
        r#"let limited = burrow_github::quota::limited(&response);"#,
    ] {
        assert_eq!(reads_a_limit(line), None, "{line}");
    }
}
