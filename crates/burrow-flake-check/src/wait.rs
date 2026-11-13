//! A PR leg that would build derivations cold, while the push to the default
//! branch it is merged onto is still building (and will keep) them, waits for
//! that push's kept builds instead of compiling the same thing in parallel
//! (#863). GitHub scopes a PR's cache to its own ref, so a push to the default
//! branch is the only run whose kept builds a PR can restore; the workflow
//! restores them again once this says they are there.
use super::*;
use std::time::{Duration, Instant};

/// How the wait for the base's push run ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The push run kept this leg's builds: restore them.
    Saved,
    /// The push run ended and kept nothing: build cold.
    Gone,
    /// Too long: build cold.
    TimedOut,
}

/// Poll `state` (is the kept cache there, is the push run still going) until
/// the cache appears, the run is over, or `max` has passed.
pub fn poll(
    max: Duration,
    every: Duration,
    mut state: impl FnMut() -> Result<(bool, bool)>,
) -> Result<Outcome> {
    let start = Instant::now();
    loop {
        let (saved, running) = state()?;
        if saved {
            return Ok(Outcome::Saved);
        }
        if !running {
            return Ok(Outcome::Gone);
        }
        if start.elapsed() >= max {
            return Ok(Outcome::TimedOut);
        }
        std::thread::sleep(every);
    }
}

/// Whether a run of this workflow is still going in the runs API's answer.
pub fn running(runs: &str, workflow: &str) -> Result<bool> {
    Ok(jq(
        runs.as_bytes(),
        &format!(
            "[.workflow_runs[] | select(.name == {} and .status != \"completed\")] | length > 0",
            quote(workflow)
        ),
    )? == "true")
}

/// Whether the caches API's answer has an entry.
pub fn cached(caches: &str) -> Result<bool> {
    Ok(jq(caches.as_bytes(), "(.actions_caches | length) > 0")? == "true")
}

fn gh(path: &str) -> Result<String> {
    string(cmd("gh", &["api", path]))
}

fn out(k: &str, v: &str) -> Result<()> {
    if let Some(p) = env::var_os("GITHUB_OUTPUT") {
        writeln!(
            fs::OpenOptions::new().append(true).create(true).open(p)?,
            "{k}={v}"
        )?;
    }
    Ok(())
}

/// The step before the gate: find what this leg would build cold (the gate's
/// own planning, stopped before it builds), and if the base's push run is
/// still going and will keep it, wait for that, then say so.
pub fn wait() -> Result<bool> {
    if !gate::gate()? {
        return Ok(true); // the gate itself will say why
    }
    let cold = fs::read_to_string(temp().join("cold.txt")).unwrap_or_default();
    if cold.trim().is_empty() {
        println!("nothing to build cold, so nothing to wait for");
        return Ok(true);
    }
    let (repo, wf) = (var("GITHUB_REPOSITORY"), var("GITHUB_WORKFLOW"));
    // The merge step's commit has the base's tip as its first parent, and a
    // push there keeps under a key ending in that commit.
    let tip = string(cmd("git", &["rev-parse", "HEAD^1"]))?;
    let key = format!("{}{tip}", var("KEPT_KEY"));
    let runs = format!("repos/{repo}/actions/runs?event=push&head_sha={tip}&per_page=100");
    if !running(&gh(&runs)?, &wf)? {
        println!("{tip} has no run of {wf} going, so nothing is on its way");
        return Ok(true);
    }
    println!("waiting for {wf}'s push at {tip} to keep the builds this leg would compile:\n{cold}");
    let max = var("WAIT_MAX").parse().unwrap_or(3600);
    let caches = format!(
        "repos/{repo}/actions/caches?key={key}&ref=refs/heads/{}",
        var("DEFAULT_BRANCH")
    );
    let outcome = poll(Duration::from_secs(max), Duration::from_secs(30), || {
        Ok((cached(&gh(&caches)?)?, running(&gh(&runs)?, &wf)?))
    })?;
    println!("{outcome:?}");
    if outcome == Outcome::Saved {
        out("key", &key)?;
        out("saved", "true")?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    const RUNS: &str = r#"{"workflow_runs":[{"name":"flake","status":"in_progress"},{"name":"other","status":"completed"}]}"#;
    #[test]
    fn running_is_this_workflows_unfinished_run() {
        assert!(running(RUNS, "flake").unwrap());
        assert!(!running(RUNS, "other").unwrap());
        assert!(!running(RUNS, "missing").unwrap());
    }
    #[test]
    fn cached_is_any_entry() {
        assert!(cached(r#"{"actions_caches":[{"key":"k"}]}"#).unwrap());
        assert!(!cached(r#"{"actions_caches":[]}"#).unwrap());
    }
    #[test]
    fn waits_for_the_keep_then_restores_and_never_past_the_run_or_the_limit() {
        let z = Duration::ZERO;
        let mut n = 0;
        let o = poll(Duration::from_secs(60), z, || {
            n += 1;
            Ok((n == 3, true))
        })
        .unwrap();
        assert_eq!((o, n), (Outcome::Saved, 3));
        assert_eq!(
            poll(Duration::from_secs(60), z, || Ok((false, false))).unwrap(),
            Outcome::Gone
        );
        assert_eq!(poll(z, z, || Ok((false, true))).unwrap(), Outcome::TimedOut);
    }
}
