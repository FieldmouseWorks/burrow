//! The command as the house action runs it: its environment, and gh, stood
//! in for by a script that answers from files and logs every call.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::json;

const GH: &str = r#"#!/bin/sh
printf '%s\n' "$*" >> "$DIR/gh.log"
case "$*" in *'--input -') printf 'body %s\n' "$(cat)" >> "$DIR/gh.log" ;; esac
answer() { printf 'HTTP/2.0 %s\r\nContent-Type: application/json\r\n\r\n' "$1"; }
if [ -e "$DIR/gone" ]; then answer '404 Not Found'; echo '{"message":"Not Found"}'; exit 1; fi
if [ -e "$DIR/blip" ]; then rm "$DIR/blip"; answer '502 Bad Gateway'; exit 1; fi
case "$*" in
  'api -i -X GET repos/sam/r/pulls/7') answer '200 OK'; cat "$DIR/pr.json" ;;
  'api -i -X GET repos/sam/r/compare/ba5e...4ead?per_page=1') answer '200 OK'; cat "$DIR/compare.json" ;;
  'api -i -X GET repos/sam/r/contents/my%20dir/big.rs?ref=4ead')
    answer '200 OK'
    printf '{"encoding": "base64", "content": "%s"}' "$(base64 "$DIR/big.rs" | sed 's/$/\\n/' | tr -d '\n')" ;;
  'api -i -X POST repos/sam/r/statuses/4ead --input -') answer '201 Created'; echo '{}' ;;
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
    let posts: Vec<serde_json::Value> = calls
        .iter()
        .filter_map(|c| c.strip_prefix("body "))
        .map(|b| serde_json::from_str(b).unwrap())
        .collect();
    assert_eq!(posts.len(), 5, "{calls:#?}");
    let context = |p: &serde_json::Value| p["context"] == "house/file-size (#7)";
    let size = posts.iter().find(|p| context(p)).unwrap();
    assert_eq!(size["state"], "failure", "{size}");
    assert_eq!(
        size["target_url"], "https://github.example/sam/r/actions/runs/99",
        "{size}"
    );
    assert!(
        posts
            .iter()
            .filter(|p| !context(p))
            .all(|p| p["state"] == "success"),
        "{posts:#?}"
    );
    assert_eq!(calls.last().unwrap(), "api -i -X GET repos/sam/r/pulls/7");
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

// GitHub not answering once: asked again, the run goes on (#477).
#[test]
fn blip() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    setup(d, false);
    fs::write(d.join("blip"), "").unwrap();
    let out = house(d, &["check"], "true");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = log(d);
    assert_eq!(
        calls[..2],
        ["api -i -X GET repos/sam/r/pulls/7"; 2],
        "{calls:#?}"
    );
}

// GitHub's no, a bad input, a bad command: the run fails, saying why, and
// posts nothing.
#[test]
fn failures() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    setup(d, false);
    fs::write(d.join("gone"), "").unwrap();
    let out = house(d, &["check"], "true");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("HTTP 404: Not Found"));
    assert_eq!(log(d), ["api -i -X GET repos/sam/r/pulls/7"]);
    fs::remove_file(d.join("gone")).unwrap();
    fs::remove_file(d.join("gh.log")).unwrap();
    let out = house(d, &["check"], "maybe");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("MODEL_NAMES"));
    let out = house(d, &[], "true");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("usage"));
    assert!(log(d).is_empty(), "{:#?}", log(d));
}
