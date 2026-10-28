//! burrow's GitHub Actions step (.github/actions/flaky): `start` before a
//! gate, `file` after it.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::github::{Api, Error};
use crate::quarantine::{Quarantine, Quarantined, now};
use crate::run::lines;
use crate::{filing, quarantine, report};

/// The job's environment, as GitHub Actions sets it.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<String>;

fn need(env: Env, name: &str) -> Result<String, Error> {
    env(name).filter(|v| !v.is_empty()).ok_or_else(|| {
        Error(format!(
            "{name} isn't set: this runs as a step of a GitHub Actions job"
        ))
    })
}

/// Where the step keeps its files, for the job's later steps.
fn dir(env: Env) -> Result<PathBuf, Error> {
    Ok(PathBuf::from(need(env, "RUNNER_TEMP")?).join("flaky"))
}

/// The tests whose quarantine has run out to a file, and FLAKY_EXPIRED and
/// FLAKY_REPORT (an empty file) set for the job's later steps; and which
/// issue held each test, for `file`. A quarantine it can't read is a
/// warning, and both are set empty: which tests' quarantine has run out is
/// unknown, so the gate gives no test a second try.
pub fn start(api: &mut dyn Api, env: Env, cache: Option<&Path>) -> Result<(), Error> {
    let repo = need(env, "GITHUB_REPOSITORY")?;
    let dir = dir(env)?;
    let (x, r) = (dir.join("expired"), dir.join("report.jsonl"));
    let q = match quarantine::list(api, &repo, cache, now()) {
        Ok(q) => q,
        Err(e) => {
            println!(
                "::warning title=Flaky tests::{}",
                data(&format!(
                    "Couldn't read the quarantine, so no test gets a second try this run: {e}"
                ))
            );
            return append(&need(env, "GITHUB_ENV")?, "FLAKY_EXPIRED=\nFLAKY_REPORT=\n");
        }
    };
    let io = |e: std::io::Error| Error(format!("{}: {e}", dir.display()));
    fs::create_dir_all(&dir).map_err(io)?;
    fs::write(&x, lines(&q.expired)).map_err(io)?;
    let held: String = q
        .held
        .iter()
        .map(|t| format!("{} {} {}\n", t.issue, t.test, t.author))
        .collect();
    fs::write(dir.join("held"), held).map_err(io)?;
    fs::write(&r, "").map_err(io)?;
    append(
        &need(env, "GITHUB_ENV")?,
        &format!(
            "FLAKY_EXPIRED={}\nFLAKY_REPORT={}\n",
            x.display(),
            r.display()
        ),
    )?;
    match quarantine::tests(&q.held) {
        t if t.is_empty() => println!("No tests quarantined in {repo}."),
        t => println!("Quarantined in {repo}: {}", t.join(", ")),
    }
    if !q.expired.is_empty() {
        println!(
            "Quarantine run out, so a failure fails at once: {}",
            q.expired.join(", ")
        );
    }
    Ok(())
}

/// The gate's report on its tests' issues, an annotation for each, and the
/// job's summary.
pub fn file(api: &mut dyn Api, env: Env, cache: Option<&Path>) -> Result<(), Error> {
    let repo = need(env, "GITHUB_REPOSITORY")?;
    let path = match env("FLAKY_REPORT") {
        None => {
            return Err(Error(
                "FLAKY_REPORT isn't set: run burrow's flaky step with `step: start` before the gate"
                    .into(),
            ));
        }
        Some(p) if p.is_empty() => {
            println!(
                "Nothing to file: the quarantine couldn't be read, so no test had a second try."
            );
            return Ok(());
        }
        Some(p) => p,
    };
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error(format!("{path}: {e}"))),
    };
    let entries = report::parse(&text).map_err(|e| Error(format!("{path}, {e}")))?;
    if entries.is_empty() {
        println!("No flaky tests.");
        return Ok(());
    }
    let event = event(env);
    let head = event["pull_request"]["head"]["repo"]["full_name"].as_str();
    if let Some(fork) = head.filter(|h| !h.eq_ignore_ascii_case(&repo)) {
        // A fork's run has a token that can't open issues, and its report is
        // the fork's own code talking: what it says is shown, never filed.
        let mut summary =
            format!("### Flaky tests\n\nNot filed: this PR comes from a fork, {fork}.\n\n");
        let mut seen = Vec::new();
        for e in entries.iter().filter(|e| {
            let new = !seen.contains(&&e.test);
            seen.push(&e.test);
            new
        }) {
            println!(
                "::warning title={TITLE}::{}",
                data(&format!("{} {FLAKED}", e.test))
            );
            summary += &format!("- `{}` {FLAKED}\n", e.test);
        }
        if let Some(s) = env("GITHUB_STEP_SUMMARY").filter(|v| !v.is_empty()) {
            append(&s, &summary)?;
        }
        return Ok(());
    }
    let start = started(&dir(env)?)?;
    let open = quarantine::list(api, &repo, cache, now())?.held;
    let filed = filing::file(api, &repo, &entries, &start, &open, &place(env, &event)?)?;
    let server = need(env, "GITHUB_SERVER_URL")?;
    let mut summary = String::from("### Flaky tests\n\n");
    for f in &filed {
        let how = if f.new { "quarantined by" } else { "noted on" };
        println!(
            "::warning title={TITLE}::{}",
            data(&format!("{} {FLAKED}: {how} #{}", f.test, f.issue))
        );
        summary += &format!(
            "- `{}` {FLAKED}: {how} [#{}]({server}/{repo}/issues/{})\n",
            f.test, f.issue, f.issue
        );
    }
    if let Some(s) = env("GITHUB_STEP_SUMMARY").filter(|v| !v.is_empty()) {
        append(&s, &summary)?;
    }
    Ok(())
}

/// The quarantine `start` gave the gate: none if it hasn't run.
fn started(dir: &Path) -> Result<Quarantine, Error> {
    let read = |name: &str| {
        let path = dir.join(name);
        match fs::read_to_string(&path) {
            Ok(t) => Ok((path, t)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((path, String::new())),
            Err(e) => Err(Error(format!("{}: {e}", path.display()))),
        }
    };
    let (path, text) = read("held")?;
    let held = text
        .lines()
        .map(|l| {
            let mut f = l.split(' ');
            match (
                f.next().and_then(|n| n.parse().ok()),
                f.next(),
                f.next(),
                f.next(),
            ) {
                (Some(issue), Some(test), Some(author), None) => Ok(Quarantined {
                    test: test.into(),
                    issue,
                    author: author.into(),
                }),
                _ => Err(Error(format!(
                    "{}: {l:?} isn't ISSUE TEST AUTHOR",
                    path.display()
                ))),
            }
        })
        .collect::<Result<_, _>>()?;
    let expired = read("expired")?.1.lines().map(String::from).collect();
    Ok(Quarantine { held, expired })
}

/// Where the job ran, in Markdown: its run (and try), the PR and its head,
/// or the branch and commit.
fn place(env: Env, event: &Value) -> Result<String, Error> {
    let (server, repo, id) = (
        need(env, "GITHUB_SERVER_URL")?,
        need(env, "GITHUB_REPOSITORY")?,
        need(env, "GITHUB_RUN_ID")?,
    );
    let attempt = env("GITHUB_RUN_ATTEMPT").unwrap_or_else(|| "1".into());
    let workflow = env("GITHUB_WORKFLOW")
        .unwrap_or_else(|| "its workflow".into())
        .replace(['[', ']'], "");
    let job = env("GITHUB_JOB")
        .map(|j| format!("`{}` in ", j.replace('`', "'")))
        .unwrap_or_default();
    let pr = &event["pull_request"];
    let on = match (pr["number"].as_u64(), pr["head"]["sha"].as_str()) {
        (Some(n), Some(sha)) => format!("PR #{n} at `{}`", short(sha)),
        _ => format!(
            "`{}` at `{}`",
            need(env, "GITHUB_REF_NAME")?.replace('`', "'"),
            short(&need(env, "GITHUB_SHA")?)
        ),
    };
    let again = if attempt == "1" {
        String::new()
    } else {
        format!(", try {attempt}")
    };
    Ok(format!(
        "{job}[{workflow}, run {id}{again}]({server}/{repo}/actions/runs/{id}/attempts/{attempt}), on {on}"
    ))
}

/// The event that started the run, or null.
fn event(env: Env) -> Value {
    env("GITHUB_EVENT_PATH")
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(Value::Null)
}

/// A flaky test's annotation title, and what it says the test did.
const TITLE: &str = "Flaky test";
const FLAKED: &str = "failed, then passed when it ran again";

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(12)]
}

fn append(path: &str, text: &str) -> Result<(), Error> {
    OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| Error(format!("{path}: {e}")))
}

/// A workflow command's message, escaped as GitHub Actions reads it.
pub fn data(s: &str) -> String {
    s.replace('%', "%25")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// A workflow command's property.
pub fn property(s: &str) -> String {
    data(s).replace(':', "%3A").replace(',', "%2C")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::fake::{Fake, ok};
    use crate::run::scratch;
    use crate::test::iso;
    use serde_json::json;
    use std::collections::HashMap;

    const LIST: &str = "repos/sam/game/issues?labels=flaky";

    /// A job's environment: on sam/game's own PR #3, or a fork's (`pr`
    /// names the head's repo), or a push to main (None).
    fn job(dir: &Path, pr: Option<&str>) -> HashMap<&'static str, String> {
        let event = dir.join("event.json");
        let payload = match pr {
            Some(head) => {
                json!({"pull_request": {"number": 3, "head": {"sha": "0123456789abcdef0123", "repo": {"full_name": head}}}})
            }
            None => json!({}),
        };
        fs::write(&event, payload.to_string()).unwrap();
        HashMap::from([
            ("GITHUB_REPOSITORY", "sam/game".to_string()),
            ("RUNNER_TEMP", dir.display().to_string()),
            ("GITHUB_ENV", dir.join("env").display().to_string()),
            (
                "GITHUB_STEP_SUMMARY",
                dir.join("summary").display().to_string(),
            ),
            ("GITHUB_SERVER_URL", "https://github.com".to_string()),
            ("GITHUB_RUN_ID", "77".to_string()),
            ("GITHUB_RUN_ATTEMPT", "2".to_string()),
            ("GITHUB_WORKFLOW", "verify".to_string()),
            ("GITHUB_JOB", "gate".to_string()),
            ("GITHUB_EVENT_PATH", event.display().to_string()),
            ("GITHUB_REF_NAME", "main".to_string()),
            ("GITHUB_SHA", "fedcba9876543210fedc".to_string()),
        ])
    }

    #[test]
    fn start_then_file() {
        let dir = scratch("flaky-actions");
        fs::create_dir_all(&dir).unwrap();
        let mut vars = job(&dir, Some("Sam/Game"));
        let mut gh = Fake::default();
        // `held` quarantined for a minute, `stale` for three days.
        let at = |n, test: &str, ago: u64| {
            json!({"number": n, "title": format!("flaky: {test}"), "labels": [{"name": "flaky"}],
                   "user": {"login": "github-actions[bot]", "type": "Bot"}, "created_at": iso(now() - ago)})
        };
        let held = json!([at(4, "held", 60), at(5, "stale", 3 * 86400)]);
        // Its fix merges while the gate runs.
        gh.on(
            "GET",
            LIST,
            vec![ok(200, held, &[]), ok(200, json!([]), &[])],
        );
        gh.on(
            "GET",
            "repos/sam/game/issues/",
            vec![ok(200, json!([]), &[])],
        );
        gh.on(
            "POST",
            "repos/sam/game/issues/4/comments",
            vec![ok(201, json!({}), &[])],
        );
        gh.on(
            "POST",
            "repos/sam/game/labels",
            vec![ok(201, json!({}), &[])],
        );
        gh.on(
            "POST",
            "repos/sam/game/issues",
            vec![ok(201, json!({"number": 9}), &[])],
        );
        {
            let env = |k: &str| vars.get(k).cloned();
            start(&mut gh, &env, None).unwrap();
        }
        let set = fs::read_to_string(dir.join("env")).unwrap();
        let x = dir.join("flaky/expired");
        let r = dir.join("flaky/report.jsonl");
        assert_eq!(
            set,
            format!(
                "FLAKY_EXPIRED={}\nFLAKY_REPORT={}\n",
                x.display(),
                r.display()
            )
        );
        assert_eq!(fs::read_to_string(&x).unwrap(), "stale\n");
        assert_eq!(
            fs::read_to_string(dir.join("flaky/held")).unwrap(),
            "4 held github-actions[bot]\n5 stale github-actions[bot]\n"
        );
        // The gate reports two flaky tests, one of them quarantined.
        fs::write(&r, "{\"test\": \"wobbly\"}\n{\"test\": \"held\"}\n").unwrap();
        vars.insert("FLAKY_REPORT", r.display().to_string());
        let env = |k: &str| vars.get(k).cloned();
        file(&mut gh, &env, None).unwrap();
        let new = gh
            .asked("POST")
            .into_iter()
            .find(|p| p.1 == "repos/sam/game/issues")
            .unwrap();
        let body = new.2.as_ref().unwrap()["body"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(
            body.starts_with("`wobbly` failed, then passed when it ran again on the same tree, in `gate` in [verify, run 77, try 2](https://github.com/sam/game/actions/runs/77/attempts/2), on PR #3 at `0123456789ab`."),
            "{body}"
        );
        assert_eq!(
            fs::read_to_string(dir.join("summary")).unwrap(),
            "### Flaky tests\n\n- `wobbly` failed, then passed when it ran again: quarantined by [#9](https://github.com/sam/game/issues/9)\n- `held` failed, then passed when it ran again: noted on [#4](https://github.com/sam/game/issues/4)\n"
        );
        // Noted where it was held: no new issue quarantines it again.
        let posts: Vec<&str> = gh.asked("POST").iter().map(|p| p.1.as_str()).collect();
        assert_eq!(
            posts,
            [
                "repos/sam/game/labels",
                "repos/sam/game/issues",
                "repos/sam/game/issues/4/comments"
            ]
        );
        // A gate that passes a test whose quarantine ran out on a second
        // try files nothing, and fails.
        fs::write(&r, "{\"test\": \"stale\"}\n").unwrap();
        let e = file(&mut gh, &env, None).unwrap_err();
        assert!(e.0.contains("`stale`"), "{e}");
        assert_eq!(gh.asked("POST").len(), 3);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn on_a_branch_and_with_nothing_to_file() {
        let dir = scratch("flaky-actions");
        fs::create_dir_all(&dir).unwrap();
        let vars = job(&dir, None);
        let env = |k: &str| vars.get(k).cloned();
        assert_eq!(
            place(&env, &event(&env)).unwrap(),
            "`gate` in [verify, run 77, try 2](https://github.com/sam/game/actions/runs/77/attempts/2), on `main` at `fedcba987654`"
        );
        // An empty report, or none (the gate never ran), asks GitHub nothing.
        let mut gh = Fake::default();
        let mut vars = vars.clone();
        vars.insert("FLAKY_REPORT", dir.join("absent").display().to_string());
        let env = |k: &str| vars.get(k).cloned();
        file(&mut gh, &env, None).unwrap();
        assert!(gh.asked.is_empty());
        // Without start, it says so.
        vars.remove("FLAKY_REPORT");
        let env = |k: &str| vars.get(k).cloned();
        assert!(
            file(&mut gh, &env, None)
                .unwrap_err()
                .0
                .contains("step: start")
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_quarantine_it_cant_read_means_no_second_tries() {
        let dir = scratch("flaky-actions");
        fs::create_dir_all(&dir).unwrap();
        let mut vars = job(&dir, Some("sam/game"));
        let mut gh = Fake::default();
        gh.on(
            "GET",
            LIST,
            vec![ok(401, json!({"message": "Bad credentials"}), &[])],
        );
        {
            let env = |k: &str| vars.get(k).cloned();
            start(&mut gh, &env, None).unwrap();
        }
        assert_eq!(
            fs::read_to_string(dir.join("env")).unwrap(),
            "FLAKY_EXPIRED=\nFLAKY_REPORT=\n"
        );
        vars.insert("FLAKY_REPORT", String::new());
        let env = |k: &str| vars.get(k).cloned();
        file(&mut gh, &env, None).unwrap();
        assert_eq!(gh.asked.len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_forks_pr_shows_what_it_would_file() {
        let dir = scratch("flaky-actions");
        fs::create_dir_all(&dir).unwrap();
        let mut vars = job(&dir, Some("stranger/game"));
        let r = dir.join("report.jsonl");
        fs::write(
            &r,
            "{\"test\": \"wobbly\"}\n{\"test\": \"wobbly\"}\n{\"test\": \"held\"}\n",
        )
        .unwrap();
        vars.insert("FLAKY_REPORT", r.display().to_string());
        let env = |k: &str| vars.get(k).cloned();
        let mut gh = Fake::default();
        file(&mut gh, &env, None).unwrap();
        assert!(gh.asked.is_empty());
        assert_eq!(
            fs::read_to_string(dir.join("summary")).unwrap(),
            "### Flaky tests\n\nNot filed: this PR comes from a fork, stranger/game.\n\n- `wobbly` failed, then passed when it ran again\n- `held` failed, then passed when it ran again\n"
        );
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn escapes() {
        assert_eq!(data("50% done\r\nnext"), "50%25 done%0D%0Anext");
        assert_eq!(property("a: b, c"), "a%3A b%2C c");
    }
}
