//! The branch watch: deleting the branches nothing needs any more, on what
//! changes them (a push, a PR closing, the default moving, the wait it was
//! given ending), with a stale one's commits archived first. Every pass
//! looks, at no cost to the rate limit while nothing changed. A full pass
//! also tries again what GitHub said no to: a stale one's archive, a
//! delete, or a read a judgment needed.
//!
//! What it keeps is in repo-REPO.json, beside what settle and the steward
//! keep there, which stays as it was.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::LazyLock;

use burrow_github::{call, encode};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::config::Branches;
use crate::pass::{DAY, Pass, repo_file, unavailable};

/// Unmerged, with no PR and no commit for this many days: given up on
/// (agents open a PR when they push). Its commits are kept under ARCHIVE
/// and the branch deleted, so a short wait costs nothing.
pub const STALE_DAYS: u32 = 3;
const ARCHIVE: &str = "refs/archive";

/// A branch no open PR has goes only once it has sat at one tip this long,
/// so a PR opened (or reopened) on it just after its PR closed, or the
/// first commit on one just pushed at the default's tip, keeps it.
pub const GRACE: f64 = 3600.0;

/// What it lists, under these headings.
pub fn unkept() -> String {
    format!("Stale branches it couldn't archive (unmerged, no PR, {STALE_DAYS}+ days)")
}
pub const UNDELETED: &str = "Branches GitHub wouldn't let it delete";

/// What the branch watch keeps, in repo-REPO.json.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct State {
    /// Each branch it may delete, by name, as it was last judged.
    pub tips: BTreeMap<String, Tip>,
    /// The last listing of the branches: its etag, what they were sorted
    /// by then (the default, and the globs kept), and the default's name
    /// and tip, which every comparison was against.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches_by: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<Value>,
    /// The last listing of the closed PRs: its etag, and its newest change.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_etag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closed_since: Option<f64>,
    /// What settle and the steward keep here.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// A branch it may delete, at the tip it was listed at.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Tip {
    pub sha: String,
    /// When it was first seen at this tip, which its wait runs from: none
    /// while an open PR has it (`pr`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seen: Option<f64>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pr: bool,
    /// When it's due a look again. None: only on a push, a PR or the
    /// default moving (a dependency bot's work in progress, one GitHub said
    /// no about).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub due: Option<f64>,
    /// GitHub said no to a read its judgment needed: judged again on a full
    /// pass.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub retry: bool,
    /// Stale, and GitHub wouldn't archive it: listed, and tried again on a
    /// full pass.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unkept: bool,
    /// What GitHub said when it refused its delete: listed, and tried again
    /// on a full pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub undeleted: Option<String>,
}

/// What the pass knows of the repo: its default branch, the branches its
/// open PRs have (head and base, drafts and forks' PRs included: deleting
/// a base closes the PRs on it), and whether it's a full pass.
pub struct Repo<'a> {
    pub default: &'a str,
    pub open: &'a BTreeSet<String>,
    pub full: bool,
}

/// Deleting a branch on GitHub only while its tip is still the one judged:
/// git's lease, since the REST API has no conditional delete.
pub trait Push {
    /// What git did; Err when it couldn't run.
    fn delete(&mut self, repo: &str, name: &str, sha: &str) -> Result<Pushed, String>;
}

pub struct Pushed {
    /// Its exit code (None: a signal ended it).
    pub code: Option<i32>,
    pub said: String,
}

/// git, from a bare repo of its own, with gh as its only credential helper:
/// it pushes as GH_TOKEN, the App's token for the repo's owner.
pub struct Git {
    pub bin: PathBuf,
}

impl Push for Git {
    fn delete(&mut self, repo: &str, name: &str, sha: &str) -> Result<Pushed, String> {
        let dir = tempfile::tempdir().map_err(|e| format!("a git dir to push from: {e}"))?;
        let init = Command::new(&self.bin)
            .args(["init", "-q", "--bare"])
            .arg(dir.path())
            .output()
            .map_err(|e| format!("{}: {e}", self.bin.display()))?;
        if !init.status.success() {
            let why = String::from_utf8_lossy(&init.stderr);
            return Err(format!("git init: {} {}", init.status, why.trim()));
        }
        let out = Command::new(&self.bin)
            .arg("-C")
            .arg(dir.path())
            .args(["-c", "credential.helper="])
            .args(["-c", "credential.helper=!gh auth git-credential"])
            .args(["push", "-q"])
            .arg(format!("--force-with-lease=refs/heads/{name}:{sha}"))
            .arg(format!("https://github.com/{repo}.git"))
            .arg(format!(":refs/heads/{name}"))
            .output()
            .map_err(|e| format!("{}: {e}", self.bin.display()))?;
        Ok(Pushed {
            code: out.status.code(),
            said: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }
}

/// Delete the branches nothing needs any more, on what changes them: a push
/// to one (the listing changes), a PR of its closing (it leaves the open
/// PRs' branches, or is among the PRs closed since the last look), the
/// default moving under it, or the wait it was given ending (the pass it
/// books). A pass GitHub stopped answering keeps what it judged and leaves
/// the rest to the next one. What it couldn't archive or delete is listed.
pub fn watch(p: &mut Pass, cfg: &Branches, repo: &Repo, git: &mut dyn Push) {
    let file = repo_file(p.repo);
    let mut st: State = p.load(&file);
    if let Err(why) = look(p, cfg, repo, git, &mut st) {
        eprintln!("{}: its branches, left for the next pass: {why}", p.repo);
    }
    if let Err(e) = p.save(&file, &st) {
        p.out.failed.push(format!("its branches: {e}"));
    }
    for tip in st.tips.values().filter(|t| !t.unkept) {
        if let Some(due) = tip.due {
            // ends: that branch's look, when it goes or has a push or a PR since
            p.wake(due);
        }
    }
    let repo = p.repo;
    for (name, tip) in &st.tips {
        if tip.unkept {
            p.list(&unkept(), format!("{repo}: `{name}`"), String::new());
        } else if let Some(why) = &tip.undeleted {
            p.list(UNDELETED, format!("{repo}: `{name}`"), why.clone());
        }
    }
}

/// Each branch judged that's due it. Err when GitHub didn't list the
/// branches or the closed PRs (it said no, or didn't answer), or stopped
/// answering partway: what was judged by then is kept, and the listings'
/// etags and base only once every branch is judged by them, so the next
/// pass judges the rest by them too.
fn look(
    p: &mut Pass,
    cfg: &Branches,
    repo: &Repo,
    git: &mut dyn Push,
    st: &mut State,
) -> Result<(), String> {
    // The machine's globs, plus the repo's own (evidence branches it cites),
    // and its train's candidates', which cn makes and deletes.
    let keep = cfg.keep(p.repo, p.cfg.settle.rules(p.repo).train);
    // The last listing stands for this one while it's unchanged, unless
    // what it's sorted by has changed since: a branch no longer kept, or no
    // longer the default, is judged.
    let by = json!({"default": repo.default, "keep": keep});
    let etag = match st.branches_by == Some(by.clone()) {
        true => st.branches_etag.clone(),
        false => None,
    };
    let got = listed(p, repo.default, etag.as_deref())?;
    let (now, base) = match &got {
        Some(l) => (l.unprotected.clone(), Some(l.base.clone())),
        None => {
            let kept = st.tips.iter().map(|(n, t)| (n.clone(), t.sha.clone()));
            (kept.collect(), st.base.clone())
        }
    };
    // Every comparison was against the default's tip: one that has moved (a
    // merge into it, a force-push) may judge a branch otherwise. No base
    // kept (a first look, or one that stopped partway) reads as moved.
    let moved = base != st.base;
    let closed = closed_heads(p, st.closed_since, st.closed_etag.as_deref())?;
    let heads = match &closed {
        None => Some(BTreeSet::new()),
        Some(c) => c.heads.clone(),
    };
    st.tips.retain(|name, _| now.contains_key(name)); // deleted, here or by anyone
    for (name, sha) in &now {
        if name == repo.default || keep.iter().any(|pat| glob(name, pat)) {
            st.tips.remove(name);
            continue;
        }
        if repo.open.contains(name) {
            let tip = Tip {
                sha: sha.clone(),
                pr: true,
                ..Tip::default()
            };
            st.tips.insert(name.clone(), tip);
            continue;
        }
        let tip = match st.tips.get(name) {
            // New, pushed to, or its PR just left the open ones: the wait
            // starts now.
            None => judge(p, git, repo.default, name, sha, p.now)?,
            Some(was) if was.sha != *sha || was.pr => {
                judge(p, git, repo.default, name, sha, p.now)?
            }
            // GitHub refused its delete, which nothing but a push changes
            // (the default moving or a PR closing would only try it again):
            // tried again on the full pass alone.
            Some(was) if was.undeleted.is_some() => match repo.full {
                true => judge(p, git, repo.default, name, sha, seen(was, p.now))?,
                false => continue,
            },
            // Judged again from when it was first seen at this tip, so its
            // wait comes out the same however often it's judged: a closed
            // PR's runs from the close itself (judge).
            Some(was)
                if heads
                    .as_ref()
                    .is_none_or(|h| h.contains(&(name.clone(), sha.clone())))
                    || moved
                    || looks_again(was, p.now, repo.full) =>
            {
                judge(p, git, repo.default, name, sha, seen(was, p.now))?
            }
            Some(_) => continue,
        };
        st.tips.insert(name.clone(), tip);
    }
    if let Some(l) = got {
        st.branches_etag = l.etag;
        st.branches_by = Some(by);
        st.base = Some(l.base);
    }
    if let Some(c) = closed {
        st.closed_etag = c.etag;
        st.closed_since = Some(c.newest);
    }
    Ok(())
}

/// When a branch judged before was first seen at its tip. Every judged one
/// has it; one that hasn't (an open PR's) is seen now.
fn seen(was: &Tip, now: f64) -> f64 {
    was.seen.unwrap_or(now)
}

/// Whether a branch judged before, and unchanged since, is judged again
/// this pass: its wait is over, or (a full pass) GitHub said no last time,
/// to archiving it or to a read its judgment needed.
fn looks_again(tip: &Tip, now: f64, full: bool) -> bool {
    if tip.unkept || tip.retry {
        return full;
    }
    tip.due.is_some_and(|due| due <= now)
}

/// The repo's branches, as GitHub lists them now.
struct Listed {
    /// What to ask by next time: none when they ran past a page.
    etag: Option<String>,
    /// Each unprotected one's tip, by name.
    unprotected: BTreeMap<String, String>,
    /// The default's name and tip.
    base: Value,
}

/// The repo's branches, or None when they're as `etag` saw them.
fn listed(p: &mut Pass, default: &str, etag: Option<&str>) -> Result<Option<Listed>, String> {
    let path = format!("repos/{}/branches?per_page=100", p.repo);
    let Some((mut etag, mut page, next)) = fresh(p, &path, etag)? else {
        return Ok(None);
    };
    if page.len() >= 100 {
        // More than a page: every page, and no etag to ask by next time,
        // since the first page's can't say the rest are unchanged.
        etag = None;
        if let Some(next) = next {
            page.extend(p.pages(&next)?);
        }
    }
    let tip = |b: &Value| b["commit"]["sha"].as_str().map(String::from);
    let at = page.iter().find(|b| b["name"] == default).and_then(tip);
    let unprotected = page
        .iter()
        .filter(|b| b["protected"] != true)
        .filter_map(|b| Some((b["name"].as_str()?.to_string(), tip(b)?)))
        .collect();
    Ok(Some(Listed {
        etag,
        unprotected,
        base: json!([default, at]),
    }))
}

/// What the PRs closed (or changed while closed) since the last look's
/// newest change say.
struct Closed {
    /// The etag to ask by next time, and the newest change now.
    etag: Option<String>,
    newest: f64,
    /// The branches here, at the tips they closed at. None when more
    /// changed since than a page holds, and every branch is to be judged
    /// again instead: reading on by page number could skip one a reopen
    /// moves up a page.
    heads: Option<BTreeSet<(String, String)>>,
}

/// The PRs closed since `since`, or None while nothing has changed. No
/// `since` (no look before): no heads, just where the next look starts. A
/// pass that kept no look kept no base either, so the next judges every
/// branch again (`moved`).
fn closed_heads(
    p: &mut Pass,
    since: Option<f64>,
    etag: Option<&str>,
) -> Result<Option<Closed>, String> {
    let path = format!(
        "repos/{}/pulls?state=closed&sort=updated&direction=desc&per_page=100",
        p.repo
    );
    let Some((etag, page, _)) = fresh(p, &path, etag)? else {
        return Ok(None);
    };
    let mut newest: Option<f64> = None;
    let mut newer = vec![];
    for pr in &page {
        let at = ts(pr["updated_at"].as_str().unwrap_or_default())?;
        newest = Some(newest.map_or(at, |n| n.max(at)));
        // The same second as `since` again: one changed in it may not have
        // been listed then.
        if since.is_some_and(|since| at >= since) {
            newer.push(pr);
        }
    }
    let repo = p.repo;
    let heads = (newer.len() < 100).then(|| {
        newer
            .iter()
            .filter(|pr| pr["head"]["repo"]["full_name"].as_str() == Some(repo))
            .filter_map(|pr| {
                let head = &pr["head"];
                Some((head["ref"].as_str()?.into(), head["sha"].as_str()?.into()))
            })
            .collect()
    });
    Ok(Some(Closed {
        etag,
        newest: newest.unwrap_or(since.unwrap_or(0.0)),
        heads,
    }))
}

/// A listing's first page: its etag, its list and the next page's path.
type Page = (Option<String>, Vec<Value>, Option<String>);

/// GET `path` unless it's unchanged since GitHub gave it `etag`; None when
/// it's unchanged, which GitHub doesn't count against the rate limit.
fn fresh(p: &mut Pass, path: &str, etag: Option<&str>) -> Result<Option<Page>, String> {
    let r = call(p.api, "GET", path, None, etag).map_err(|e| p.github(e))?;
    if r.status == 304 {
        return Ok(None);
    }
    let tag = r.header("etag").map(String::from);
    match r.json().map_err(|e| p.github(e))? {
        Value::Array(page) => Ok(Some((tag, page, r.next()))),
        _ => Err(format!("GET {path}: GitHub's answer isn't a list")),
    }
}

/// A branch no open PR has, at `sha` since `seen`: its record, with when
/// it's due a look again, and deleted once that's past and nothing needs
/// it. Err when GitHub didn't answer, or git couldn't run.
fn judge(
    p: &mut Pass,
    git: &mut dyn Push,
    default: &str,
    name: &str,
    sha: &str,
    seen: f64,
) -> Result<Tip, String> {
    let mut tip = Tip {
        sha: sha.into(),
        seen: Some(seen),
        ..Tip::default()
    };
    // GitHub deletes a branch when its PR merges, never when it closes.
    // Matched by the tip, so a bot's branch pushed anew since (its next
    // update, with no PR yet) or anyone's pushed after the close stays.
    let closed = closed_at(p, name, sha);
    let Some(pr) = answer(closed)? else {
        tip.retry = true;
        return Ok(tip);
    };
    let (due, why) = if let Some((n, at)) = pr {
        // An hour from the close, or from when it was first seen at this
        // tip if that's later: the same every time it's judged.
        let why = format!("#{n} was closed at it, and keeps its commits");
        (seen.max(at) + GRACE, Some(why))
    } else if name.starts_with("renovate/") || name.starts_with("dependabot/") {
        return Ok(tip); // any other branch of a dependency bot's is its work in progress
    } else {
        // Compared at the tip just listed, and deleted only if it's still there.
        let path = format!(
            "repos/{}/compare/{}...{}",
            p.repo,
            encode(default),
            encode(sha)
        );
        let cmp = p.get(&path);
        let cmp = answer(cmp)?.unwrap_or_default();
        let last = cmp["commits"].as_array().and_then(|c| c.last());
        let date = last.and_then(|c| c["commit"]["committer"]["date"].as_str());
        match (cmp["ahead_by"].as_u64(), date) {
            (Some(0), _) => (seen + GRACE, Some(format!("already in {default}"))),
            (Some(_), Some(date)) => {
                let stale = ts(date)? + f64::from(STALE_DAYS) * DAY;
                ((seen + GRACE).max(stale), None)
            }
            // Nothing to judge it by. GitHub's no reads the same whether
            // it's for now (its access) or for good (no history in common
            // with the default), so it's asked again.
            _ => {
                tip.retry = true;
                return Ok(tip);
            }
        }
    };
    tip.due = Some(due);
    if p.now < due {
        return Ok(tip);
    }
    let why = match why {
        Some(why) => why,
        // Unmerged, with no PR and no commit for STALE_DAYS. Nothing is
        // lost: one ref made from the archived one restores it.
        None => {
            if !archive(p, name, sha)? {
                tip.unkept = true;
                return Ok(tip);
            }
            format!("unmerged, no PR, {STALE_DAYS}+ days; kept at {ARCHIVE}/{name}/{sha}")
        }
    };
    eprintln!("{}: deleting {name} at {} ({why})", p.repo, short(sha));
    // Its record stays until the listing drops it. One GitHub refused is
    // listed for the pipeline and tried again on the full pass, not on every
    // pass until then.
    let refused = delete(p, git, name, sha)?;
    if !refused.is_empty() {
        tip.undeleted = Some(refused);
    }
    Ok(tip)
}

/// What GitHub answered, or None when it said no (a branch with no history
/// in common with the default has no comparison, say). Err when it didn't
/// answer, which ends the pass's look at the branches: the next pass takes
/// up the rest.
fn answer<T>(got: Result<T, String>) -> Result<Option<T>, String> {
    match got {
        Ok(v) => Ok(Some(v)),
        Err(why) if unavailable(&why) => Err(why),
        Err(why) => {
            eprintln!("  failed: {why}");
            Ok(None)
        }
    }
}

/// The closed PR whose head was this branch at `sha`, and when it last
/// closed: the latest such close. GitHub keeps that PR's commits
/// (refs/pull/N/head) and offers to restore the branch from its page, so
/// the branch holds nothing the PR doesn't. None when there's none.
fn closed_at(p: &mut Pass, name: &str, sha: &str) -> Result<Option<(u64, f64)>, String> {
    let owner = p.repo.split('/').next().unwrap_or_default();
    // Every page: a branch name reused for many PRs can have its match
    // past the first hundred.
    let path = format!(
        "repos/{}/pulls?state=closed&per_page=100&head={owner}:{}",
        p.repo,
        encode(name)
    );
    let mut last: Option<(f64, u64)> = None;
    for pr in p.pages(&path)? {
        let at = pr["closed_at"].as_str();
        let (Some(n), Some(at)) = (pr["number"].as_u64(), at) else {
            continue;
        };
        if pr["head"]["sha"] == sha {
            let close = (ts(at)?, n);
            last = Some(last.map_or(close, |l| if close > l { close } else { l }));
        }
    }
    Ok(last.map(|(at, n)| (n, at)))
}

/// Keep the branch's commits at refs/archive/NAME/SHA, a ref GitHub keeps
/// but lists with neither branches nor tags: true once they're kept (by
/// this call or an earlier one), and only then may the branch go. Err when
/// GitHub didn't answer.
fn archive(p: &mut Pass, name: &str, sha: &str) -> Result<bool, String> {
    let to = format!("{ARCHIVE}/{name}/{sha}");
    if p.dry {
        eprintln!("  would: keep {} {name} at {to}", p.repo);
        return Ok(true);
    }
    let made = json!({"ref": to, "sha": sha});
    let path = format!("repos/{}/git/refs", p.repo);
    let made = call(p.api, "POST", &path, Some(&made), None).map_err(|e| p.github(e));
    if answer(made)?.is_some() {
        return Ok(true);
    }
    // Already there: an earlier look kept it and then couldn't delete.
    let at: Vec<String> = to["refs/".len()..].split('/').map(encode).collect();
    let path = format!("repos/{}/git/ref/{}", p.repo, at.join("/"));
    let got = p.get(&path);
    Ok(answer(got)?.is_some_and(|r| r["object"]["sha"] == sha))
}

/// Read whole, shown cut: a long URL mustn't push git's verdict off the end.
const SHOWN: usize = 200;

/// The fields where git echoes names back: URLs, refs, and the names on
/// its ref-status lines.
static ECHOES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://\S+|refs/\S+|\[[a-z ]+\] \S+").expect("a regex"));
/// What git says when GitHub didn't answer, besides what any call says
/// then (pass::unavailable).
static OUTAGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)could not resolve host|returned error: 5\d\d").expect("a regex")
});

/// Delete the branch only if its tip is still `sha`: a push since then
/// keeps it. What GitHub said when it refused (a ruleset, a token that
/// can't), else "": deleted, already gone, or kept by a push the next
/// listing shows. Err when GitHub didn't answer, or git couldn't run (which
/// fails the look too).
fn delete(p: &mut Pass, git: &mut dyn Push, name: &str, sha: &str) -> Result<String, String> {
    if p.dry {
        eprintln!(
            "  would: delete {} {name} if still at {}",
            p.repo,
            short(sha)
        );
        return Ok(String::new());
    }
    let pushed = match git.delete(p.repo, name, sha) {
        Ok(pushed) => pushed,
        Err(e) => {
            p.out.failed.push(format!("its branches: {e}"));
            return Err(e);
        }
    };
    let refused = verdict(name, &pushed)?;
    if !refused.is_empty() {
        eprintln!("{}: kept {name}: {refused}", p.repo);
    }
    Ok(refused)
}

/// What git's word on a delete comes to (delete).
fn verdict(name: &str, pushed: &Pushed) -> Result<String, String> {
    let said = pushed.said.split_whitespace().collect::<Vec<_>>().join(" ");
    if pushed.code == Some(0)
        || said.contains("stale info")
        || said.contains("remote ref does not exist")
    {
        return Ok(String::new());
    }
    let shown: String = said.chars().take(SHOWN).collect();
    // Less the fields where git echoes names back, so a branch called
    // fix/timeout reads as a refusal, not an outage. Only those fields: a
    // branch called `error` mustn't take the word out of git's own
    // "returned error: 502".
    let bare = ECHOES.replace_all(&said, "");
    if unavailable(&bare) || OUTAGE.is_match(&bare) {
        return Err(format!("git push :{name}: {shown}"));
    }
    if !shown.is_empty() {
        return Ok(shown);
    }
    Ok(match pushed.code {
        Some(code) => format!("git exited {code}"),
        None => "git was killed".into(),
    })
}

/// A commit, as a log line names it.
fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// GitHub's time, in seconds.
fn ts(at: &str) -> Result<f64, String> {
    let t: jiff::Timestamp = at
        .parse()
        .map_err(|e| format!("GitHub's time {at:?}: {e}"))?;
    Ok(t.as_millisecond() as f64 / 1000.0)
}

/// Whether `name` matches the glob, as Python's fnmatch, which the
/// steward's globs were first written for, has it: `*` is any run of
/// characters, `/` among them, `?` any one, and `[...]` any one it has
/// (`[!...]`: any one it hasn't), `a-z` a range of them and a `]` first one
/// of them. A `[` with no `]` after it is itself.
fn glob(name: &str, pat: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pat: Vec<char> = pat.chars().collect();
    matches(&name, &pat)
}

fn matches(name: &[char], pat: &[char]) -> bool {
    let Some((&first, rest)) = pat.split_first() else {
        return name.is_empty();
    };
    match first {
        '*' => (0..=name.len()).any(|i| matches(&name[i..], rest)),
        '?' => !name.is_empty() && matches(&name[1..], rest),
        '[' => match class(rest) {
            Some((class, after)) => {
                name.first().is_some_and(|&c| class.has(c)) && matches(&name[1..], after)
            }
            None => name.first() == Some(&'[') && matches(&name[1..], rest),
        },
        c => name.first() == Some(&c) && matches(&name[1..], rest),
    }
}

/// A `[...]`, from just after its `[`.
struct Class {
    not: bool,
    ranges: Vec<(char, char)>,
}

impl Class {
    fn has(&self, c: char) -> bool {
        self.ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi) != self.not
    }
}

/// The class, and the glob after its `]`; None when there's no `]`.
fn class(pat: &[char]) -> Option<(Class, &[char])> {
    let not = pat.first() == Some(&'!');
    let start = usize::from(not);
    let mut end = start + usize::from(pat.get(start) == Some(&']'));
    while *pat.get(end)? != ']' {
        end += 1;
    }
    let body = &pat[start..end];
    let mut ranges = vec![];
    let mut i = 0;
    while i < body.len() {
        if body.get(i + 1) == Some(&'-') && i + 2 < body.len() {
            ranges.push((body[i], body[i + 2]));
            i += 3;
        } else {
            ranges.push((body[i], body[i]));
            i += 1;
        }
    }
    Some((Class { not, ranges }, &pat[end + 1..]))
}

#[cfg(test)]
#[path = "branches_world.rs"]
mod world;

#[cfg(test)]
#[path = "branches_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "branches_git_tests.rs"]
mod git_tests;
