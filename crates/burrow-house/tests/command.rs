//! The command as the house action runs it: its environment, and gh, stood
//! in for by a script that answers from files and logs every call.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::json;

const GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$DIR/gh.log"
[ ! -e "$DIR/down" ] || { echo 'HTTP 502' >&2; exit 1; }
case "$*" in
  'api repos/sam/r/pulls/7') cat "$DIR/pr.json" ;;
  'api repos/sam/r/compare/ba5e...4ead?per_page=1') cat "$DIR/compare.json" ;;
  'api -H Accept: application/vnd.github.raw repos/sam/r/contents/my%20dir/big.rs?ref=4ead') cat "$DIR/big.rs" ;;
  'api -X POST repos/sam/r/statuses/4ead '*) echo '{}' ;;
  *) echo "unexpected: $*" >&2; exit 1 ;;
esac
"#;

fn setup(d: &Path, fork: bool) {
    let gh = d.join("bin/gh");
    fs::create_dir(d.join("bin")).unwrap();
    fs::write(&gh, GH).unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
    let repo = if fork { "fork/r" } else { "sam/r" };
    let pr = json!({
        "title": "tools: a plain sentence", "body": "",
        "head": {"sha": "4ead", "repo": {"full_name": repo}},
        "base": {"sha": "ba5e", "ref": "main"},
    });
    fs::write(d.join("pr.json"), pr.to_string()).unwrap();
    let files = [json!({
        "status": "modified", "filename": "my dir/big.rs", "additions": 20, "deletions": 5,
        "patch": format!("@@ -1,5 +1,20 @@{}{}", "\n-x".repeat(5), "\n+x".repeat(20)),
    })];
    let compare = json!({"merge_base_commit": {"sha": "3e7b"}, "files": files});
    fs::write(d.join("compare.json"), compare.to_string()).unwrap();
    fs::write(d.join("big.rs"), "x\n".repeat(1001)).unwrap();
}

fn house(d: &Path, args: &[&str], model_names: &str) -> Output {
    let path = format!(
        "{}:{}",
        d.join("bin").display(),
        std::env::var("PATH").unwrap()
    );
    Command::new(env!("CARGO_BIN_EXE_burrow-house"))
        .args(args)
        .env_clear()
        .env("PATH", path)
        .env("DIR", d)
        .env("GITHUB_REPOSITORY", "sam/r")
        .env("GITHUB_SERVER_URL", "https://github.example")
        .env("GITHUB_RUN_ID", "99")
        .env("NUMBER", "7")
        .env("HEAD_SHA", "4ead")
        .env("GROWTH", "50")
        .env("LINES", "1000")
        .env("MODEL_NAMES", model_names)
        .output()
        .unwrap()
}

fn log(d: &Path) -> Vec<String> {
    fs::read_to_string(d.join("gh.log"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

// A PR that grows a code file past the limit: the file is read at the head
// by its path, sent whole, and each rule's status is posted on the head,
// linking the run, the run itself passing.
#[test]
fn posts() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    setup(d, false);
    let out = house(d, &["check"], "true");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("::error title=Why not split?::"),
        "{stdout}"
    );
    let calls = log(d);
    let posts: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("api -X POST"))
        .collect();
    assert_eq!(posts.len(), 5, "{calls:#?}");
    let context = |c: &str| c.contains("-f context=house/file-size (#7) ");
    let size = posts.iter().find(|c| context(c)).unwrap();
    assert!(
        size.contains("-f state=failure ")
            && size.ends_with("-f target_url=https://github.example/sam/r/actions/runs/99"),
        "{size}"
    );
    assert!(
        posts
            .iter()
            .filter(|c| !context(c))
            .all(|c| c.contains("-f state=success ")),
        "{posts:#?}"
    );
    assert_eq!(calls.last().unwrap(), "api repos/sam/r/pulls/7");
}

// A fork's PR: the verdict is the run's result, and nothing is posted.
#[test]
fn fork() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    setup(d, true);
    let out = house(d, &["check"], "false");
    assert_eq!(out.status.code(), Some(1));
    assert!(!log(d).iter().any(|c| c.contains("POST")), "{:#?}", log(d));
}

// GitHub not answering, a bad input, a bad command: the run fails, saying
// why, and posts nothing.
#[test]
fn failures() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    setup(d, false);
    fs::write(d.join("down"), "").unwrap();
    let out = house(d, &["check"], "true");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("HTTP 502"));
    assert_eq!(log(d), ["api repos/sam/r/pulls/7"]);
    fs::remove_file(d.join("down")).unwrap();
    fs::remove_file(d.join("gh.log")).unwrap();
    let out = house(d, &["check"], "maybe");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("MODEL_NAMES"));
    let out = house(d, &[], "true");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
    assert!(log(d).is_empty(), "{:#?}", log(d));
}
