//! A pass: each manifest's follows brought to where they should be. A follow
//! whose upstream has something that counts past the pin has one open PR,
//! from its own branch (`branch`), moving it to the upstream's head; one
//! with nothing that counts has none.

use std::io::Write;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use burrow_github::{Api, Error, call, call_allowing};
use serde_json::{Value, json};

use crate::config::{Config, Manifest};
use crate::local::Local;
use crate::manifest::{self, Follow};

/// What the pass was asked: one repo's follows (the follower's, or the
/// ones on an upstream), or every one; and whether only to say what it
/// would do.
pub struct Ask<'a> {
    pub only: Option<&'a str>,
    pub dry: bool,
}

/// How a pass's PR title, and its one commit's message, start.
const TITLE: &str = "deps: follow ";

/// How many changed paths a PR lists.
const LISTED: usize = 30;

/// The branch a manifest's follow of an upstream keeps its PR on:
/// `follow/OWNER/NAME/PATH`, one per pair, so two manifests in a repo that
/// follow one upstream never share a PR.
fn branch(upstream: &str, path: &str) -> String {
    format!("follow/{}/{path}", upstream.to_ascii_lowercase())
}

fn short(rev: &str) -> &str {
    &rev[..7.min(rev.len())]
}

fn get(api: &mut dyn Api, path: &str) -> Result<Value, Error> {
    call(api, "GET", path, None, None).and_then(|r| r.json())
}

fn send(api: &mut dyn Api, method: &str, path: &str, body: &Value) -> Result<Value, Error> {
    call(api, method, path, Some(body), None).and_then(|r| r.json())
}

fn field<'a>(v: &'a Value, path: &str, name: &str) -> Result<&'a str, Error> {
    v.pointer(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::said(format!("{path}: GitHub's answer has no {name}")))
}

/// A repo's name as GitHub spells it, and its default branch's head.
fn repo_head(api: &mut dyn Api, repo: &str) -> Result<(String, String, String), Error> {
    let path = format!("repos/{repo}");
    let r = get(api, &path)?;
    let name = field(&r, &path, "/full_name")?.to_string();
    let branch = field(&r, &path, "/default_branch")?.to_string();
    let path = format!("repos/{name}/git/ref/heads/{branch}");
    let head = field(&get(api, &path)?, &path, "/object/sha")?.to_string();
    Ok((name, branch, head))
}

/// A file at a commit, or None when it has none there.
fn file(api: &mut dyn Api, repo: &str, name: &str, at: &str) -> Result<Option<String>, Error> {
    let path = format!("repos/{repo}/contents/{name}?ref={at}");
    let r = call_allowing(api, "GET", &path, None, None, &[404])?;
    if r.status == 404 {
        return Ok(None);
    }
    let v = r.json()?;
    let content: String = field(&v, &path, "/content")?
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    let bytes = STANDARD
        .decode(content)
        .map_err(|e| Error::said(format!("{path}: its content isn't base64: {e}")))?;
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| Error::said(format!("{path}: not UTF-8")))
}

/// The open PR from the follow's branch.
struct Open {
    number: u64,
    head: String,
    labels: Vec<String>,
}

fn open_pr(api: &mut dyn Api, repo: &str, branch: &str) -> Result<Option<Open>, Error> {
    let owner = repo.split('/').next().unwrap_or_default();
    let path = format!("repos/{repo}/pulls?state=open&head={owner}:{branch}");
    let r = get(api, &path)?;
    let Some(pr) = r.as_array().and_then(|a| a.first()) else {
        return Ok(None);
    };
    Ok(Some(Open {
        number: pr["number"]
            .as_u64()
            .ok_or_else(|| Error::said(format!("{path}: GitHub's answer has no number")))?,
        head: field(pr, &path, "/head/sha")?.to_string(),
        labels: pr["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str().map(String::from))
            .collect(),
    }))
}

/// How a pass went.
#[derive(Debug, PartialEq, Eq)]
pub struct Ran {
    /// Every manifest it reached went through.
    pub ok: bool,
    /// When GitHub's rate limit, which held off some manifest and nothing
    /// worse, lifts first: the pass is due again then, not failed.
    pub held: Option<u64>,
}

/// Every manifest, or the ones the ask is about. Each failure is said, and
/// the rest go on.
pub fn run(
    cfg: &Config,
    ask: &Ask,
    api: &mut dyn Api,
    local: &mut dyn Local,
    out: &mut dyn Write,
) -> Ran {
    let mut ran = Ran {
        ok: true,
        held: None,
    };
    for m in &cfg.manifests {
        let mut p = Pass {
            cfg,
            ask,
            api: &mut *api,
            local: &mut *local,
            out: &mut *out,
        };
        match p.manifest(m) {
            Ok(()) => {}
            Err(Error {
                retry_at: Some(at),
                why,
                ..
            }) => {
                let _ = writeln!(out, "{}:{}: held off until {at}: {why}", m.repo, m.path);
                ran.held = Some(ran.held.map_or(at, |h| h.min(at)));
            }
            Err(e) => {
                let _ = writeln!(out, "{}:{}: {e}", m.repo, m.path);
                ran.ok = false;
            }
        }
    }
    ran
}

struct Pass<'a> {
    cfg: &'a Config,
    ask: &'a Ask<'a>,
    api: &'a mut dyn Api,
    local: &'a mut dyn Local,
    out: &'a mut dyn Write,
}

/// Where a follow's PR goes.
struct Target<'a> {
    repo: &'a str,
    branch: &'a str,
    base: &'a str,
    path: &'a str,
}

impl Pass<'_> {
    fn say(&mut self, line: String) {
        let _ = writeln!(self.out, "{line}");
    }

    fn manifest(&mut self, m: &Manifest) -> Result<(), Error> {
        let (repo, branch, base) = repo_head(self.api, &m.repo)?;
        let text = file(self.api, &repo, &m.path, &base)?
            .ok_or_else(|| Error::said(format!("{repo} has no {} at {}", m.path, short(&base))))?;
        let target = Target {
            repo: &repo,
            branch: &branch,
            base: &base,
            path: &m.path,
        };
        let mut failed = Vec::new();
        for f in manifest::follows(&text)? {
            let about =
                |r: &str| r.eq_ignore_ascii_case(&repo) || r.eq_ignore_ascii_case(&f.upstream);
            if self.ask.only.is_some_and(|o| !about(o)) {
                continue;
            }
            if let Err(e) = self.follow(&target, &f) {
                failed.push(e.context(&f.upstream));
            }
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(Error::aggregate(failed))
        }
    }

    fn follow(&mut self, t: &Target, f: &Follow) -> Result<(), Error> {
        let (upstream, up_branch, head) = repo_head(self.api, &f.upstream)?;
        let branch = branch(&upstream, t.path);
        let open = open_pr(self.api, t.repo, &branch)?;
        let counted = if head == f.pin {
            Vec::new()
        } else {
            self.local.changes(f, &f.pin, &head)?
        };
        let (repo, path) = (t.repo, t.path);
        if let Some(pr) = &open
            && let Some(l) = pr.labels.iter().find(|l| self.cfg.frozen.contains(l))
        {
            self.say(format!(
                "{repo}: #{} has {l:?}, so it's left as it is",
                pr.number
            ));
            return Ok(());
        }
        if counted.is_empty() {
            let why = format!(
                "{upstream}'s {up_branch} at {} changes nothing {path} builds from it past {}, the pin.",
                short(&head),
                short(&f.pin)
            );
            self.say(format!("{repo}: {why}"));
            if let Some(pr) = open {
                if let Some(theirs) = self.others(repo, pr.number)? {
                    self.say(format!(
                        "{repo}: #{} has a commit a pass didn't make ({theirs:?}), so it's left open",
                        pr.number
                    ));
                } else if self.ask.dry {
                    self.say(format!("{repo}: would close #{}", pr.number));
                } else {
                    self.close(repo, &branch, pr.number, &why)?;
                    self.say(format!("{repo}: closed #{}", pr.number));
                }
            }
            return Ok(());
        }
        // Already at the head, or at a rev the head changes nothing past
        // that counts (an inert commit on top of what the PR takes), and
        // GitHub doesn't say it conflicts: a moved base or an inert commit
        // alone is no reason for a new head (its gate would start over).
        if let Some(pr) = &open
            && let Some(at) = self.moves_to(repo, path, &pr.head, &f.url)?
            && (at == head || (at != f.pin && self.local.changes(f, &at, &head)?.is_empty()))
            && !self.conflicted(repo, pr.number)?
        {
            let past = if at == head {
                String::new()
            } else {
                format!(", and {} changes nothing past it that counts", short(&head))
            };
            self.say(format!(
                "{repo}: #{} already moves {upstream} to {}{past}",
                pr.number,
                short(&at)
            ));
            return Ok(());
        }
        // Commits on it that aren't a pass's (the steward's fix job making
        // the follower build against the new head): a new head would throw
        // them away. It lands as it is, and the push that lands it starts
        // the pass that moves it on.
        if let Some(pr) = &open
            && let Some(theirs) = self.others(repo, pr.number)?
        {
            self.say(format!(
                "{repo}: #{} has a commit a pass didn't make ({theirs:?}), so it's left as it is",
                pr.number
            ));
            return Ok(());
        }
        if self.ask.dry {
            self.say(format!(
                "{repo}: would move {upstream} to {} ({})",
                short(&head),
                counted.join(", ")
            ));
            return Ok(());
        }
        let files = self.local.bump(repo, t.base, path, f, &head)?;
        if files.is_empty() {
            return Err(Error::said(format!(
                "moving {path} to {} changed no file",
                short(&head)
            )));
        }
        let title = format!("{TITLE}{upstream} to {}", short(&head));
        let sha = self.commit(repo, t.base, &files, &title)?;
        self.point(repo, &branch, &sha)?;
        let body = body(&upstream, &up_branch, f, &head, path, &counted);
        let pr = json!({ "title": title, "body": body });
        match open {
            Some(pr_open) => {
                send(
                    self.api,
                    "PATCH",
                    &format!("repos/{repo}/pulls/{}", pr_open.number),
                    &pr,
                )?;
                self.say(format!(
                    "{repo}: #{} now moves {upstream} to {}",
                    pr_open.number,
                    short(&head)
                ));
            }
            None => {
                let mut pr = pr;
                pr["head"] = json!(branch);
                pr["base"] = json!(t.branch);
                let path = format!("repos/{repo}/pulls");
                let made = send(self.api, "POST", &path, &pr)?;
                let url = field(&made, &path, "/html_url")?.to_string();
                self.say(format!(
                    "{repo}: {url} moves {upstream} to {}",
                    short(&head)
                ));
            }
        }
        Ok(())
    }

    /// The rev the manifest at `at` pins the URL at.
    fn moves_to(
        &mut self,
        repo: &str,
        path: &str,
        at: &str,
        url: &str,
    ) -> Result<Option<String>, Error> {
        let Some(text) = file(self.api, repo, path, at)? else {
            return Ok(None);
        };
        let follows = manifest::follows(&text).unwrap_or_default();
        Ok(follows.into_iter().find(|f| f.url == url).map(|f| f.pin))
    }

    /// The first line of the first commit on the PR that a pass didn't make.
    fn others(&mut self, repo: &str, number: u64) -> Result<Option<String>, Error> {
        let path = format!("repos/{repo}/pulls/{number}/commits?per_page=100");
        let commits = get(self.api, &path)?;
        let commits = commits.as_array().ok_or_else(|| {
            Error::said(format!("{path}: GitHub's answer isn't a list of commits"))
        })?;
        Ok(commits.iter().find_map(|c| {
            let message = c["commit"]["message"].as_str().unwrap_or_default();
            (!message.starts_with(TITLE))
                .then(|| message.lines().next().unwrap_or_default().to_string())
        }))
    }

    /// Whether GitHub says the PR can't merge as it is.
    fn conflicted(&mut self, repo: &str, number: u64) -> Result<bool, Error> {
        let pr = get(self.api, &format!("repos/{repo}/pulls/{number}"))?;
        Ok(pr["mergeable"] == json!(false))
    }

    /// One commit on `base` with the files, signed by GitHub as whoever
    /// the pass acts as.
    fn commit(
        &mut self,
        repo: &str,
        base: &str,
        files: &[(String, Vec<u8>)],
        message: &str,
    ) -> Result<String, Error> {
        let path = format!("repos/{repo}/git/commits/{base}");
        let tree = field(&get(self.api, &path)?, &path, "/tree/sha")?.to_string();
        let mut entries = Vec::new();
        for (name, bytes) in files {
            let content = std::str::from_utf8(bytes)
                .map_err(|_| Error::said(format!("{name}: not UTF-8")))?;
            entries.push(
                json!({ "path": name, "mode": "100644", "type": "blob", "content": content }),
            );
        }
        let path = format!("repos/{repo}/git/trees");
        let made = send(
            self.api,
            "POST",
            &path,
            &json!({ "base_tree": tree, "tree": entries }),
        )?;
        let tree = field(&made, &path, "/sha")?.to_string();
        let path = format!("repos/{repo}/git/commits");
        let made = send(
            self.api,
            "POST",
            &path,
            &json!({ "message": message, "tree": tree, "parents": [base] }),
        )?;
        Ok(field(&made, &path, "/sha")?.to_string())
    }

    /// The branch at the commit, made or moved.
    fn point(&mut self, repo: &str, branch: &str, sha: &str) -> Result<(), Error> {
        let made = json!({ "ref": format!("refs/heads/{branch}"), "sha": sha });
        let path = format!("repos/{repo}/git/refs");
        let r = call_allowing(self.api, "POST", &path, Some(&made), None, &[422])?;
        if r.status == 422 {
            let moved = json!({ "sha": sha, "force": true });
            send(
                self.api,
                "PATCH",
                &format!("repos/{repo}/git/refs/heads/{branch}"),
                &moved,
            )?;
        }
        Ok(())
    }

    /// The PR closed with why, and its branch deleted.
    fn close(&mut self, repo: &str, branch: &str, number: u64, why: &str) -> Result<(), Error> {
        send(
            self.api,
            "POST",
            &format!("repos/{repo}/issues/{number}/comments"),
            &json!({ "body": why }),
        )?;
        send(
            self.api,
            "PATCH",
            &format!("repos/{repo}/pulls/{number}"),
            &json!({ "state": "closed" }),
        )?;
        let path = format!("repos/{repo}/git/refs/heads/{branch}");
        call_allowing(self.api, "DELETE", &path, None, None, &[404, 422])?;
        Ok(())
    }
}

fn body(
    upstream: &str,
    up_branch: &str,
    f: &Follow,
    head: &str,
    path: &str,
    counted: &[String],
) -> String {
    let deps = f
        .deps
        .iter()
        .map(|d| format!("`{d}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut listed: String = counted
        .iter()
        .take(LISTED)
        .map(|p| format!("- `{p}`\n"))
        .collect();
    if counted.len() > LISTED {
        listed += &format!("- and {} more\n", counted.len() - LISTED);
    }
    format!(
        "{upstream}'s {up_branch} is at {head}. This moves {deps} in `{path}` there from {pin}, and the lock file with them.\n\n\
         What changed there that `{path}`'s `metadata.follow` doesn't call inert:\n\n{listed}\n\
         https://github.com/{upstream}/compare/{pin}...{head}\n\n\
         cargo-follow keeps this PR at {upstream}'s head: a commit there that changes only what the manifest calls inert \
         (its pin on this repo, say) doesn't refresh it, and with nothing past the pin that counts, it closes it.\n",
        pin = f.pin,
    )
}

#[cfg(test)]
#[path = "pass_tests.rs"]
mod tests;
