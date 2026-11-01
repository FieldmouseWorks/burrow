//! HOST-update's PR, in a repo whose PRs land through its merge train:
//! nothing but the train moves its main, so the tested commit goes up as a
//! PR from the train's `branch`, for the steward to settle and the train to
//! land (Conary's docs/DESIGN.md, section 4.5), never as a push.
//!
//! While that PR is open, HOST-update brings in nothing fresh. A new head
//! would take it out of the train, and stop the candidate it's in and every
//! PR in that with it; a newer bump put live beside it would leave the box on
//! something main never gets. So it rides the PR instead (`ride`): main with
//! the PR's head on top, which is what the train lands next.
//!
//! Unless a check failed on a head it pushed itself (`red`). The steward
//! settles only a green PR, and takes one that goes red out of the train, so
//! that head is in no candidate for a new one to stop, and riding it would
//! hold the box until a Claude job got to it. So it brings in a fresh bump as
//! if none were open, which goes up as the same PR's new head (`replace`):
//! only a whole one, though, that isn't the red head's very tree
//! (`replaces`).
//!
//! The head it pushed last is `proposed`, in its state dir: a head on its PR
//! that isn't that one is someone else's, a fix, and what it holds hasn't
//! been through HOST-update's gate.

use std::fmt;
use std::fs;

use burrow_github::{Api, call, encode, query};
use serde_json::{Value, json};

use crate::{Config, Train, is_commit};

/// HOST-update's open PR, as GitHub listed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pr {
    pub number: u64,
    /// Its head when it was listed.
    pub head: String,
}

impl Pr {
    /// One as update.sh hands it over, "NUMBER HEAD".
    pub fn parse(said: &str) -> Option<Pr> {
        let (n, head) = said.split_once(' ')?;
        let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
        if !digits(n) || !is_commit(head) {
            return None;
        }
        Some(Pr {
            number: n.parse().ok()?,
            head: head.to_string(),
        })
    }
}

impl fmt::Display for Pr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{} {}", self.number, self.head)
    }
}

/// Its open PR, None when there's none. Fails when GitHub doesn't say. Only
/// the repo's own branch counts: a fork's PR from a branch of the same name
/// is anyone's to open. GitHub's head filter is OWNER:BRANCH, and allows one
/// open PR per head and base, so there's never a page more; a fork of the
/// owner's own under another name has a head of the same name, so the
/// head's repo is the repo's too.
pub fn open(t: &Train, api: &mut dyn Api) -> Result<Option<Pr>, String> {
    let (owner, _) = parts(t);
    let path = format!(
        "repos/{}/pulls?state=open&base=main&head={}:{}",
        t.repo,
        encode(owner),
        encode(&t.branch)
    );
    let listed = call(api, "GET", &path, None, None)?.json()?;
    let Some(listed) = listed.as_array() else {
        return Err(format!("GET {path}: not a list of PRs"));
    };
    let ours = listed.iter().find(|p| {
        p["head"]["repo"]["full_name"]
            .as_str()
            .is_some_and(|r| r.eq_ignore_ascii_case(&t.repo))
    });
    let Some(p) = ours else { return Ok(None) };
    let said = format!(
        "{} {}",
        p["number"].as_u64().unwrap_or(0),
        p["head"]["sha"].as_str().unwrap_or("")
    );
    Pr::parse(&said)
        .map(Some)
        .ok_or_else(|| format!("GET {path}: a PR that isn't one ({said})"))
}

/// Whether its open PR `pr` is red on a head HOST-update pushed, with that
/// head fetched when it is (`replaces` reads it): false when it isn't, or
/// when the head is anyone else's (`ride` holds for that). Red is CI as the
/// steward reads it (ci_state in modules/autoland/prs.py, whose rules these
/// are), in `failing`. Fails, saying why, when GitHub doesn't say, or the
/// head moved since `pr` was listed.
pub fn red(cfg: &Config, t: &Train, api: &mut dyn Api, pr: &Pr) -> Result<bool, String> {
    if proposed(cfg).as_deref() != Some(pr.head.as_str()) {
        return Ok(false);
    }
    let (owner, name) = parts(t);
    let vars = json!({"owner": owner, "name": name, "n": pr.number});
    let data = query(api, owner, CHECKS, vars).map_err(|e| {
        eprintln!("{e}");
        format!("GitHub didn't say how its PR #{}'s checks stand", pr.number)
    })?;
    let p = &data["repository"]["pullRequest"];
    if p["headRefOid"].as_str() != Some(pr.head.as_str()) {
        return Err(format!(
            "its PR #{}'s head isn't {} any more",
            pr.number, pr.head
        ));
    }
    if !failing(t, &p["commits"]["nodes"][0]["commit"]) {
        return Ok(false);
    }
    fetch(cfg, t, pr)?;
    Ok(true)
}

/// A PR's head commit's checks, as `failing` reads them.
const CHECKS: &str = "query($owner: String!, $name: String!, $n: Int!) {
  repository(owner: $owner, name: $name) { pullRequest(number: $n) { headRefOid
    commits(last: 1) { nodes { commit {
      statusCheckRollup { contexts(first: 100) { pageInfo { hasNextPage } nodes {
        __typename
        ... on CheckRun { status conclusion }
        ... on StatusContext { context state creator { login __typename } }
      } } }
      checkSuites(first: 100) { pageInfo { hasNextPage } nodes { conclusion } }
    } } }
  } }
}";

/// Whether CI on `commit` (as GraphQL gives it) is red: a check whose latest
/// run failed (so one rerunning is pending), a workflow that failed to
/// start, or a status that's neither success nor pending, except the
/// steward's own (`settled`) and the train's (`context`), which counts only
/// as a failure from the steward's App (`steward`): the head red merged
/// alone. Over 100 checks or suites isn't red: the steward doesn't read
/// past them either.
fn failing(t: &Train, commit: &Value) -> bool {
    let contexts = &commit["statusCheckRollup"]["contexts"];
    let suites = &commit["checkSuites"];
    if [contexts, suites]
        .iter()
        .any(|x| x["pageInfo"]["hasNextPage"] == true)
    {
        return false;
    }
    let nodes = |x: &Value| x["nodes"].as_array().cloned().unwrap_or_default();
    let started = |s: &Value| s["conclusion"] != "STARTUP_FAILURE";
    let red = |c: &Value| {
        if c["__typename"] == "CheckRun" {
            c["status"] == "COMPLETED"
                && !matches!(
                    c["conclusion"].as_str(),
                    Some("SUCCESS" | "NEUTRAL" | "SKIPPED")
                )
        } else if c["context"] == t.settled.as_str() {
            false
        } else if c["context"] == t.context.as_str() {
            c["state"] == "FAILURE"
                && c["creator"]["__typename"] == "Bot"
                && c["creator"]["login"] == t.steward.as_str()
        } else {
            !matches!(
                c["state"].as_str(),
                Some("SUCCESS" | "PENDING" | "EXPECTED")
            )
        }
    };
    !nodes(suites).iter().all(started) || nodes(contexts).iter().any(red)
}

/// Main with its open PR `pr` on top, in the clone, which update.sh has at
/// main. Fails, saying why, when that head isn't the one HOST-update pushed
/// (someone's fixing it: what it holds hasn't been through HOST-update's
/// gate) or doesn't merge onto main, with the clone as it was.
pub fn ride(cfg: &Config, t: &Train, pr: &Pr) -> Result<(), String> {
    if proposed(cfg).as_deref() != Some(pr.head.as_str()) {
        return Err(format!(
            "its PR #{} has a head it didn't push ({}); holding until that lands",
            pr.number, pr.head
        ));
    }
    fetch(cfg, t, pr)?;
    let msg = format!("Ride #{} on main", pr.number);
    if cfg
        .git_hushed(&["merge", "-q", "--no-edit", "-m", &msg, "FETCH_HEAD"])
        .is_err()
    {
        if cfg.git_hushed(&["merge", "--abort"]).is_err() {
            let _ = cfg.git(&["reset", "-q", "--hard"]);
        }
        return Err(format!(
            "its PR #{} doesn't merge onto main; holding until it's fixed",
            pr.number
        ));
    }
    Ok(())
}

/// Whether HEAD, a fresh bump on `main` for its red PR `pr`, replaces that
/// head: Ok, or why it doesn't. `short` are the inputs that didn't make it
/// in (they didn't build, or what's new in them couldn't be fetched). One
/// short of any, or with nothing new on main at all, could take back what
/// that head put live; one that's that head's very tree would only go red
/// the same way again. Either way the Claude job on the red PR (or a rerun)
/// has it. A tree it can't read is a reason too: it never replaces a head
/// it couldn't tell apart.
pub fn replaces(cfg: &Config, pr: &Pr, main: &str, short: &[String]) -> Result<(), String> {
    let tree = |c: &str| {
        cfg.git(&["rev-parse", "--verify", "-q", &format!("{c}^{{tree}}")])
            .map_err(|e| format!("a fresh bump can't be told apart from it ({e})"))
    };
    let fresh = tree("HEAD")?;
    if !short.is_empty() {
        Err(format!("a fresh bump is short of {}", short.join(" ")))
    } else if fresh == tree(main)? {
        Err("a fresh bump has nothing new on main".into())
    } else if fresh == tree(&pr.head)? {
        Err("a fresh bump is that same tree".into())
    } else {
        Ok(())
    }
}

/// HEAD as its red PR `pr`'s new head, retitled for HEAD (`said`): the same
/// PR, never a new one. Only while that head is still on it and still red: a
/// head pushed since is someone's fix, and one gone green (a rerun) is the
/// steward's to settle and put in the train, so it's ridden instead. Gives
/// the PR's URL; fails, saying why, with the PR left as it was. update.sh
/// calls it once HEAD is live and healthy but before it's confirmed, so a
/// refusal reverts it: the box never stays on a bump nothing will land.
pub fn replace(cfg: &Config, t: &Train, api: &mut dyn Api, pr: &Pr) -> Result<String, String> {
    let (title, body) = said(cfg)?;
    let n = pr.number;
    match red(cfg, t, api, pr) {
        Err(why) => return Err(format!("{why}; its PR #{n} stays as it is")),
        Ok(false) => {
            return Err(format!(
                "its PR #{n} isn't red any more, so it stays as it is"
            ));
        }
        Ok(true) => {}
    }
    let lease = format!("--force-with-lease=refs/heads/{}:{}", t.branch, pr.head);
    let to = format!("HEAD:refs/heads/{}", t.branch);
    cfg.git_hushed(&["push", "-q", &lease, "origin", &to])
        .map_err(|_| {
            format!(
                "its PR #{n} moved on from {} while this ran, so it stays as it is",
                pr.head
            )
        })?;
    remember(cfg);
    let path = format!("repos/{}/pulls/{n}", t.repo);
    let new = json!({"title": title, "body": body});
    let url = call(api, "PATCH", &path, Some(&new), None)
        .and_then(|r| r.json())
        .ok()
        .and_then(|r| r["html_url"].as_str().map(String::from))
        .unwrap_or_else(|| {
            format!(
                "https://github.com/{}/pull/{n} (its title still says the red head's: GitHub didn't take the new one)",
                t.repo
            )
        });
    Ok(format!("{url}, in place of its red head {}", &pr.head[..7]))
}

/// HEAD as a new PR, titled and described for HEAD (`said`), whose URL it
/// gives. Only with none open: whatever was on its branch (a PR merged or
/// closed) is HOST-update's to replace.
pub fn propose(cfg: &Config, t: &Train, api: &mut dyn Api) -> Result<String, String> {
    let (title, body) = said(cfg)?;
    cfg.git(&[
        "push",
        "-q",
        "--force",
        "origin",
        &format!("HEAD:refs/heads/{}", t.branch),
    ])?;
    remember(cfg);
    let new = json!({"title": title, "head": t.branch, "base": "main", "body": body});
    let made = call(
        api,
        "POST",
        &format!("repos/{}/pulls", t.repo),
        Some(&new),
        None,
    )?
    .json()?;
    made["html_url"]
        .as_str()
        .map(String::from)
        .ok_or_else(|| "GitHub opened its PR, but didn't say where".into())
}

/// HEAD's PR's title and body: HEAD's subject, and what HOST-update did
/// with the commit, as its message says.
pub fn said(cfg: &Config) -> Result<(String, String), String> {
    let title = cfg.git(&["log", "-1", "--format=%s"])?;
    let did = cfg.git(&["log", "-1", "--format=%b"])?;
    let body = format!(
        "{did}\n\nIt is live on {}. The steward settles this PR and the merge train lands it; until then {} brings in nothing newer, unless a check fails on it: then a fresh bump replaces it.",
        cfg.host, cfg.update
    );
    Ok((title, body))
}

/// Its PR's head, fetched into the clone: only the one it was listed at.
fn fetch(cfg: &Config, t: &Train, pr: &Pr) -> Result<(), String> {
    let couldnt = || format!("couldn't fetch its PR #{}'s head ({})", pr.number, pr.head);
    let branch = format!("refs/heads/{}", t.branch);
    cfg.git(&["fetch", "-q", "origin", &branch])
        .map_err(|_| couldnt())?;
    match cfg.git(&["rev-parse", "FETCH_HEAD"]) {
        Ok(h) if h == pr.head => Ok(()),
        _ => Err(couldnt()),
    }
}

/// The head HOST-update last pushed to its PR's branch, if it has.
fn proposed(cfg: &Config) -> Option<String> {
    let said = fs::read_to_string(cfg.state.join("proposed")).ok()?;
    Some(said.trim_end_matches('\n').to_string())
}

/// HEAD as the head HOST-update last pushed. A push it can't remember goes
/// on all the same, as someone else's head: its PR is held for until it
/// lands.
fn remember(cfg: &Config) {
    let at = cfg.state.join("proposed");
    if let Err(e) = cfg
        .git(&["rev-parse", "HEAD"])
        .and_then(|h| fs::write(&at, format!("{h}\n")).map_err(|e| e.to_string()))
    {
        eprintln!("{}: {e}", at.display());
    }
}

/// The repo's owner and name.
fn parts(t: &Train) -> (&str, &str) {
    t.repo.split_once('/').unwrap_or((&t.repo, ""))
}

#[cfg(test)]
#[path = "train_tests.rs"]
mod tests;
