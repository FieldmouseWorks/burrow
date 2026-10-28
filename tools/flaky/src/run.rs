//! `flaky run`: a gate run outside GitHub Actions (on a machine of the
//! owner's, say), between reading the quarantine and filing its report.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::github::Api;
use crate::{filing, quarantine, report};

/// Run `cmd` with FLAKY_EXPIRED and FLAKY_REPORT set, then file what it
/// reported. Its exit code is the command's; one that passed but whose
/// flaky tests couldn't be filed (or whose report can't be read) fails,
/// since a passing try only counts once it's on its issue. If the
/// quarantine can't be read, neither is set: which tests' quarantine has
/// run out is unknown, so no test gets a second try.
pub fn run(
    api: &mut dyn Api,
    repo: &str,
    place: Option<&str>,
    cmd: &[String],
    cache: Option<&Path>,
) -> i32 {
    let mut gate = Command::new(&cmd[0]);
    gate.args(&cmd[1..]);
    let q = match quarantine::list(api, repo, cache, quarantine::now()) {
        Ok(q) => q,
        Err(e) => {
            eprintln!(
                "flaky: couldn't read {repo}'s quarantine, so no test gets a second try: {e}"
            );
            gate.env_remove("FLAKY_EXPIRED").env_remove("FLAKY_REPORT");
            return code(&cmd[0], gate.status());
        }
    };
    let dir = scratch("flaky-run");
    let (x, r) = (dir.join("expired"), dir.join("report.jsonl"));
    if let Err(e) = fs::create_dir_all(&dir)
        .and_then(|_| fs::write(&x, lines(&q.expired)))
        .and_then(|_| fs::write(&r, ""))
    {
        eprintln!("flaky: {}: {e}", dir.display());
        return 1;
    }
    for t in quarantine::tests(&q.held) {
        eprintln!("flaky: quarantined: {t}");
    }
    for t in &q.expired {
        eprintln!("flaky: quarantine run out, so a failure fails at once: {t}");
    }
    gate.env("FLAKY_EXPIRED", &x).env("FLAKY_REPORT", &r);
    let mut code = code(&cmd[0], gate.status());
    let shown = cmd.join(" ").replace('`', "'");
    let place = match place {
        Some(p) => p.to_string(),
        None => match head() {
            Some(sha) => format!("a run of `{shown}` at `{sha}`"),
            None => format!("a run of `{shown}`"),
        },
    };
    let filed = fs::read_to_string(&r)
        .map_err(|e| format!("{}: {e}", r.display()))
        .and_then(|t| report::parse(&t).map_err(|e| format!("its report, {e}")))
        .and_then(|entries| {
            if entries.is_empty() {
                return Ok(Vec::new());
            }
            let open = quarantine::list(api, repo, cache, quarantine::now())
                .map_err(|e| e.to_string())?
                .held;
            filing::file(api, repo, &entries, &q, &open, &place).map_err(|e| e.to_string())
        });
    match filed {
        Ok(f) => {
            for x in f {
                eprintln!(
                    "flaky: {} {}: {repo}#{}",
                    x.test,
                    if x.new { "quarantined by" } else { "noted on" },
                    x.issue
                );
            }
        }
        Err(e) => {
            eprintln!("flaky: couldn't file the report: {e}");
            if code == 0 {
                code = 1;
            }
        }
    }
    let _ = fs::remove_dir_all(&dir);
    code
}

/// A directory of its own under the system's temp dir.
pub fn scratch(name: &str) -> std::path::PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    std::env::temp_dir().join(format!(
        "{name}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// The tests, one a line.
pub fn lines<S: AsRef<str>>(t: &[S]) -> String {
    t.iter().map(|t| format!("{}\n", t.as_ref())).collect()
}

fn head() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The gate's exit code, or 127 if it couldn't start.
fn code(name: &str, status: std::io::Result<std::process::ExitStatus>) -> i32 {
    match status {
        Ok(s) => exit_code(s),
        Err(e) => {
            eprintln!("flaky: {name}: {e}");
            127
        }
    }
}

#[cfg(unix)]
fn exit_code(s: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0))
}

#[cfg(not(unix))]
fn exit_code(s: std::process::ExitStatus) -> i32 {
    s.code().unwrap_or(1)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::github::fake::{Fake, ok};
    use crate::test::iso;
    use serde_json::json;

    const LIST: &str = "repos/sam/game/issues?labels=flaky";

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".into(), "-c".into(), script.into()]
    }

    /// `held` quarantined for a minute, `stale` for three days.
    fn quarantined() -> serde_json::Value {
        let at = |n, test: &str, ago: u64| {
            json!({"number": n, "title": format!("flaky: {test}"), "labels": [{"name": "flaky"}],
                   "user": {"login": "github-actions[bot]", "type": "Bot"},
                   "created_at": iso(quarantine::now() - ago)})
        };
        json!([at(4, "held", 60), at(5, "stale", 3 * 86400)])
    }

    #[test]
    fn gives_the_gate_its_files_and_files_its_report() {
        let mut gh = Fake::default();
        // `held`'s fix merges while the gate runs.
        gh.on(
            "GET",
            LIST,
            vec![ok(200, quarantined(), &[]), ok(200, json!([]), &[])],
        );
        gh.on(
            "GET",
            "repos/sam/game/issues/",
            vec![ok(200, json!([]), &[])],
        );
        gh.on(
            "POST",
            "repos/sam/game/labels",
            vec![ok(201, json!({}), &[])],
        );
        gh.on(
            "POST",
            "repos/sam/game/issues/",
            vec![ok(201, json!({}), &[])],
        );
        gh.on(
            "POST",
            "repos/sam/game/issues",
            vec![ok(201, json!({"number": 9}), &[])],
        );
        // The gate reads which quarantine ran out and reports two tests that
        // passed on a second try: a new one, and `held`, which goes on the
        // issue that held it.
        let script = r#"test "$(cat "$FLAKY_EXPIRED")" = stale || exit 7
            echo '{"test": "wobbly", "detail": "first try"}' >> "$FLAKY_REPORT"
            echo '{"test": "held"}' >> "$FLAKY_REPORT""#;
        assert_eq!(
            run(
                &mut gh,
                "sam/game",
                Some("the gate on PR #3"),
                &sh(script),
                None
            ),
            0
        );
        let posts = gh.asked("POST");
        assert_eq!(
            posts.iter().map(|p| p.1.as_str()).collect::<Vec<_>>(),
            [
                "repos/sam/game/labels",
                "repos/sam/game/issues",
                "repos/sam/game/issues/4/comments"
            ]
        );
        assert!(
            posts[1].2.as_ref().unwrap()["body"]
                .as_str()
                .unwrap()
                .contains("in the gate on PR #3.")
        );
    }

    #[test]
    fn a_test_whose_quarantine_ran_out_cant_pass_on_a_second_try() {
        let mut gh = Fake::default();
        gh.on("GET", LIST, vec![ok(200, quarantined(), &[])]);
        gh.on(
            "GET",
            "repos/sam/game/issues/",
            vec![ok(200, json!([]), &[])],
        );
        let stale = sh(r#"echo '{"test": "stale"}' >> "$FLAKY_REPORT""#);
        assert_eq!(run(&mut gh, "sam/game", None, &stale, None), 1);
        assert!(gh.asked("POST").is_empty());
    }

    #[test]
    fn keeps_the_gates_exit_code() {
        let mut gh = Fake::default();
        gh.on("GET", LIST, vec![ok(200, json!([]), &[])]);
        assert_eq!(run(&mut gh, "sam/game", None, &sh("exit 3"), None), 3);
        assert_eq!(
            run(&mut gh, "sam/game", None, &sh("kill -TERM $$"), None),
            128 + 15
        );
        assert_eq!(
            run(
                &mut gh,
                "sam/game",
                None,
                &["/nonexistent/gate".into()],
                None
            ),
            127
        );
        // A clean run asks GitHub only for the quarantine.
        assert_eq!(gh.asked.len(), 3);
        assert!(gh.asked("POST").is_empty());
    }

    #[test]
    fn a_pass_it_cant_file_fails() {
        let mut gh = Fake::default();
        gh.on("GET", LIST, vec![ok(200, json!([]), &[])]);
        gh.on(
            "POST",
            "repos/sam/game/labels",
            vec![ok(403, json!({"message": "no"}), &[])],
        );
        let flaky = sh(r#"echo '{"test": "wobbly"}' >> "$FLAKY_REPORT""#);
        assert_eq!(run(&mut gh, "sam/game", None, &flaky, None), 1);
        // A report it can't read is the gate's bug: a failure too.
        assert_eq!(
            run(
                &mut gh,
                "sam/game",
                None,
                &sh(r#"echo 'not json' >> "$FLAKY_REPORT""#),
                None
            ),
            1
        );
        // A failing gate keeps its own code.
        assert_eq!(
            run(
                &mut gh,
                "sam/game",
                None,
                &sh(r#"echo '{"test": "w"}' >> "$FLAKY_REPORT"; exit 5"#),
                None
            ),
            5
        );
    }

    #[test]
    fn a_quarantine_it_cant_read_means_no_second_tries() {
        let mut gh = Fake::default();
        gh.on(
            "GET",
            LIST,
            vec![ok(401, json!({"message": "Bad credentials"}), &[])],
        );
        assert_eq!(
            run(
                &mut gh,
                "sam/game",
                None,
                &sh(r#"test -z "${FLAKY_EXPIRED+set}${FLAKY_REPORT+set}""#),
                None
            ),
            0
        );
    }
}
