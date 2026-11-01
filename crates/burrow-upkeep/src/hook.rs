//! The webhooks to burrow-events (modules/events.nix): a delivery starts its
//! handlers' passes within seconds of what happened on a repo. Each repo the
//! steward sees to has one, kept with its settings (`settle`). A full pass
//! sees to the rest (`hooks`): a hook on each repo of the machine's projects
//! the steward doesn't see to (excluded, or another owner's), for proj-watch
//! and proj-inbox; the hook off each repo it no longer sees to; and the
//! App's own, which is how a repo just made is heard. Those go as the App's
//! installation on each repo's owner, minted for here (token.rs).
//!
//! GitHub never gives a hook's secret back, so a hook is known by a hash of
//! what it was set to: a repo's in repo-REPO.json, beside what settle and
//! the branch watch keep there, and the App's in app-hook.json.

use std::collections::BTreeSet;
use std::path::PathBuf;

use burrow_github::{Api, call, call_allowing};
use gh_token::app::App;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::config::{Config, Webhook};
use crate::pass::{self, DAY, Jobs, Out, Pass, repo_file};
use crate::token::Owners;

/// What a project's repo's state says when GitHub had no such repo at its
/// last look: deleted, or out of the App's reach, which GitHub answers alike.
pub const GONE: &str = "gone";

/// What a repo's hook keeps, in repo-REPO.json.
#[derive(Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct State {
    /// What it was last set to, as `hash` has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hook: Option<String>,
    /// Its id. The hook is the one with it, so a new URL moves it rather
    /// than leaving the old one sending.
    #[serde(rename = "hookId", skip_serializing_if = "Option::is_none")]
    pub hook_id: Option<u64>,
    /// A project's repo's: when its hook was last due (daily).
    #[serde(rename = "hookAt", skip_serializing_if = "Option::is_none")]
    pub hook_at: Option<f64>,
    /// Why a repo has no hook, or keeps one it shouldn't: GONE, or what
    /// GitHub said (the App isn't installed on its owner).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unreached: Option<String>,
    /// When taking the hook off a repo no longer seen to was last tried.
    #[serde(rename = "unhookTried", skip_serializing_if = "Option::is_none")]
    pub unhook_tried: Option<f64>,
    /// What settle, the branch watch and the steward keep here.
    #[serde(flatten)]
    pub rest: Map<String, Value>,
}

/// The repo's hook: set again when its URL, events or secret change, and
/// when it's `due` (the daily settings), in case it was deleted or edited.
/// It's the one with the id it was given, else the one sending to its URL,
/// else a new one. Err when GitHub wouldn't give the repo's hooks or take
/// the change, or the state wouldn't save: it's tried again next pass.
pub fn keep(p: &mut Pass, secret: &str, st: &mut State, due: bool) -> Result<(), String> {
    let wh = &p.cfg.webhook;
    let want = wanted(wh, secret);
    if kept(st, &want, due) {
        return Ok(());
    }
    let url = wh.url.clone();
    let hook = json!({
        "active": true,
        "events": wh.events,
        "config": {"url": url, "content_type": "json", "secret": secret, "insecure_ssl": "0"},
    });
    let hooks = p
        .pages(&format!("repos/{}/hooks?per_page=100", p.repo))
        .map_err(|e| e.why)?;
    let id = |h: &Value| h["id"].as_u64();
    let mine = hooks
        .iter()
        .find(|h| st.hook_id.is_some() && id(h) == st.hook_id)
        .or_else(|| hooks.iter().find(|h| h["config"]["url"] == url.as_str()))
        .and_then(id);
    let made = match mine {
        Some(n) => {
            let path = format!("repos/{}/hooks/{n}", p.repo);
            call(p.api, "PATCH", &path, Some(&hook), None).map_err(|e| p.github(e))?;
            Some(n)
        }
        None => {
            eprintln!("{}: adding the webhook", p.repo);
            let mut add = hook;
            add["name"] = "web".into();
            let path = format!("repos/{}/hooks", p.repo);
            let got = call(p.api, "POST", &path, Some(&add), None)
                .and_then(|r| r.json())
                .map_err(|e| p.github(e))?;
            id(&got)
        }
    };
    st.hook = Some(want);
    st.hook_id = made;
    p.save(&repo_file(p.repo), st)
}

/// What a repo's hook is set to, as `hash` has it.
fn wanted(wh: &Webhook, secret: &str) -> String {
    hash(&json!([wh.url, wh.events, secret]))
}

/// Whether the hook is as it's wanted, and isn't due: nothing to ask.
fn kept(st: &State, want: &str, due: bool) -> bool {
    st.hook.as_deref() == Some(want) && !due
}

/// What a full pass hands `hooks`: the repos it sees to, and the repos the
/// machine's projects are clones of (common.py's projects), by name.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repos {
    pub seen: Vec<String>,
    pub projects: Vec<String>,
}

/// A full pass's hooks: the projects' repos', the ones off the repos no
/// longer seen to, then the App's own. What it says back is what wouldn't
/// save and a rate limit's reset; what GitHub wouldn't do is logged, and
/// tried again a day later (a project's, a gone one's) or next pass.
pub struct Hooks<'a> {
    pub cfg: &'a Config,
    /// The pass's time, which what's kept is reckoned from.
    pub now: f64,
    /// The hooks' secret, burrow-events' (systemd's credential webhook).
    pub secret: &'a str,
    pub owners: &'a mut dyn Owners,
    /// The App, as itself: its own webhook.
    pub app: &'a App,
    /// The clock, as Unix seconds, for the App's JWT.
    pub clock: i64,
    pub out: Out,
    /// What it said, a line each, on stderr too.
    pub said: Vec<String>,
}

/// Why a repo's hook couldn't be seen to.
enum Unreached {
    Gone,
    /// GitHub didn't answer: nothing new is known.
    Unanswered(String),
    /// GitHub's no: the App isn't on its owner, or to the repo's lookup.
    No(String),
}

/// What became of a hook on a repo no longer seen to.
enum Unhooked {
    Off,
    /// GitHub calls the repo this now, a live one's name, whose hook it is.
    Renamed(String),
}

impl Hooks<'_> {
    pub fn run(&mut self, repos: &Repos) {
        let seen: BTreeSet<String> = repos.seen.iter().map(|r| r.to_lowercase()).collect();
        let more = self.projects(&seen, &repos.projects);
        self.unhook(repos.seen.iter().chain(&more));
        self.app_hook();
    }

    fn say(&mut self, line: String) {
        eprintln!("{line}");
        self.said.push(line);
    }

    fn dir(&self) -> PathBuf {
        self.cfg.state_dir.clone()
    }

    fn save<T: Serialize>(&mut self, name: &str, value: &T) {
        if let Err(e) = pass::save(&self.dir(), name, value) {
            self.out.failed.push(e);
        }
    }

    /// The hooks on the projects' repos it doesn't see to. Another owner's
    /// gets one only if the App is installed there too, and a clone whose
    /// repo GitHub has no more gets none. Either is said once, then left
    /// alone a day, when it's tried again (a gone one by looking the repo
    /// up first, quietly), and the rest still get theirs. A gone repo's
    /// hook stays on record: back, that's the hook it keeps. What it gives
    /// is them all, for `unhook` to leave: a hook whose repo couldn't be
    /// asked about stays on.
    fn projects(&mut self, seen: &BTreeSet<String>, projects: &[String]) -> BTreeSet<String> {
        let more: BTreeSet<String> = projects
            .iter()
            .filter(|r| !seen.contains(&r.to_lowercase()))
            .cloned()
            .collect();
        for repo in &more {
            let name = repo_file(repo);
            let mut st: State = pass::load(&self.dir(), &name);
            let due = self.now - st.hook_at.unwrap_or(0.0) >= DAY;
            let was = st.unreached.clone();
            if was.is_some() && !due
                || was.is_none() && kept(&st, &wanted(&self.cfg.webhook, self.secret), due)
            {
                continue;
            }
            if due {
                st.hook_at = Some(self.now);
                self.save(&name, &st);
            }
            let why = match self.reach(repo, &mut st, due, was.as_deref() == Some(GONE)) {
                Ok(()) => None,
                Err(Unreached::Gone) => Some(GONE.to_string()),
                Err(Unreached::Unanswered(e)) => {
                    self.say(format!("{repo}: its webhook: {e}"));
                    was.clone()
                }
                Err(Unreached::No(e)) => Some(e),
            };
            if why == was {
                continue;
            }
            self.say(match why.as_deref() {
                Some(GONE) => format!(
                    "{repo}: GitHub has no such repo, so its clone in {} gets no webhook; \
                     looked up again daily",
                    self.cfg.projects_dir
                ),
                Some(why) => format!("{repo}: its webhook: {why}; tried again daily"),
                None if was.as_deref() == Some(GONE) => format!("{repo}: on GitHub again"),
                None => format!("{repo}: reached again"),
            });
            st.unreached = why;
            self.save(&name, &st);
        }
        more
    }

    /// A project's repo's hook, as the App's installation on its owner: a
    /// `gone` one is looked up first, and one whose hooks couldn't be read
    /// or set is gone if the repo itself is.
    fn reach(
        &mut self,
        repo: &str,
        st: &mut State,
        due: bool,
        gone: bool,
    ) -> Result<(), Unreached> {
        let (cfg, now, secret) = (self.cfg, self.now, self.secret);
        let mut jobs = NoJobs;
        let api = match self.owners.api(owner(repo)) {
            Ok(Some(api)) => api,
            Ok(None) => return Err(Unreached::No(uninstalled(cfg, repo))),
            Err(e) if e.unavailable => return Err(Unreached::Unanswered(e.what)),
            Err(e) => return Err(Unreached::No(e.what)),
        };
        let mut p = look(cfg, repo, now, api, &mut jobs);
        let mut failed = None;
        let reached = (|| {
            if gone {
                looked_up(&mut p)?;
            }
            if let Err(e) = keep(&mut p, secret, st, due) {
                failed = Some(format!("{repo}: its webhook: {e}"));
                looked_up(&mut p)?;
            }
            Ok(())
        })();
        let out = p.out;
        self.noted(out);
        if let Some(line) = failed {
            self.say(line);
        }
        reached
    }

    /// The hooks this machine gave repos it no longer sees to (excluded,
    /// archived, out of the owners), off them: their deliveries would start
    /// passes over nothing, and the URL has no business there. `live` are
    /// the repos it does see to and the projects'. One that won't come off
    /// (no admin any more, say), or whose name GitHub doesn't answer for, is
    /// tried again a day later. GitHub answers a deleted repo and one the
    /// App can't reach any more alike, so that hook stays on record, looked
    /// up again a day later, said once. A renamed repo leaves its old state
    /// file behind, and GitHub answers the old name with the new one: that
    /// hook is the one the new name's adopted, so the old file lets go of it.
    fn unhook<'r>(&mut self, live: impl Iterator<Item = &'r String>) {
        let (keep, live): (BTreeSet<String>, BTreeSet<String>) =
            live.map(|r| (repo_file(r), r.to_lowercase())).unzip();
        let mut names: Vec<String> = std::fs::read_dir(self.dir())
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n.starts_with("repo-") && n.ends_with(".json") && !keep.contains(n))
            .collect();
        names.sort();
        for name in names {
            let mut st: State = pass::load(&self.dir(), &name);
            let Some(id) = st.hook_id else {
                continue;
            };
            if self.now - st.unhook_tried.unwrap_or(0.0) < DAY {
                continue;
            }
            // An owner has no _.
            let repo = name["repo-".len()..name.len() - ".json".len()].replacen('_', "/", 1);
            match self.unhooked(&repo, id, &live, &mut st) {
                Some(Unhooked::Off) => {}
                Some(Unhooked::Renamed(now)) => {
                    self.say(format!("{repo}: renamed to {now}, which keeps the webhook"));
                }
                // Not known to be gone (the old path may lead to a live
                // repo), or its hook wouldn't come off.
                None => {
                    st.unhook_tried = Some(self.now);
                    self.save(&name, &st);
                    continue;
                }
            }
            st.hook = None;
            st.hook_id = None;
            st.unhook_tried = None;
            self.save(&name, &st);
        }
    }

    /// The repo's hook `id`, off it when GitHub's repo of that name isn't
    /// one of `live` (lower case), as the App on its owner: None when it
    /// stays, not known to be gone or not taken off.
    fn unhooked(
        &mut self,
        repo: &str,
        id: u64,
        live: &BTreeSet<String>,
        st: &mut State,
    ) -> Option<Unhooked> {
        let (cfg, now) = (self.cfg, self.now);
        let mut jobs = NoJobs;
        let api = match self.owners.api(owner(repo)) {
            Ok(Some(api)) => api,
            Ok(None) => {
                self.say(format!("{repo}: {}", uninstalled(cfg, repo)));
                return None;
            }
            Err(e) => {
                self.say(format!("{repo}: {e}"));
                return None;
            }
        };
        let mut said = vec![];
        let mut p = look(cfg, repo, now, api, &mut jobs);
        let unhooked = match looked_up(&mut p) {
            Ok(v) => {
                st.unreached = None;
                let now = v["full_name"].as_str().unwrap_or_default().to_string();
                if live.contains(&now.to_lowercase()) {
                    Some(Unhooked::Renamed(now))
                } else {
                    said.push(format!("{repo}: no longer seen to, taking its webhook off"));
                    let path = format!("repos/{repo}/hooks/{id}");
                    match call(p.api, "DELETE", &path, None, None) {
                        Ok(_) => Some(Unhooked::Off),
                        Err(e) => {
                            said.push(format!("{repo}: its webhook: {}", p.github(e)));
                            None
                        }
                    }
                }
            }
            Err(Unreached::Gone) => {
                if st.unreached.as_deref() != Some(GONE) {
                    said.push(format!(
                        "{repo}: GitHub has no such repo the App can see, so its webhook stays \
                         on record; looked up again daily"
                    ));
                }
                st.unreached = Some(GONE.into());
                None
            }
            Err(Unreached::Unanswered(e) | Unreached::No(e)) => {
                said.push(format!("{repo}: {e}"));
                None
            }
        };
        let out = p.out;
        self.noted(out);
        for line in said {
            self.say(line);
        }
        unhooked
    }

    /// The App's own webhook, to burrow-events like each repo's, with the
    /// same secret: it's how a repo just made is heard (GitHub sends the App
    /// the `repository` events of every repo its installations reach, a new
    /// one's included, before the repo has a hook of its own). The App's
    /// settings turn it on and choose its events; its URL and secret are
    /// kept here, set again when they or the App change and once a day, in
    /// case they were edited.
    fn app_hook(&mut self) {
        #[derive(Default, Serialize, Deserialize)]
        #[serde(default)]
        struct Kept {
            hook: Option<String>,
            at: f64,
        }
        let wh = &self.cfg.webhook;
        let want = hash(&json!([self.cfg.app.id, wh.url, self.secret]));
        let st: Kept = pass::load(&self.dir(), "app-hook.json");
        if st.hook.as_deref() == Some(want.as_str()) && self.now - st.at < DAY {
            return;
        }
        if let Err(e) = self.app.set_hook(&wh.url, self.secret, self.clock) {
            self.say(format!("the App's webhook: {e}"));
            return;
        }
        self.say("the App's webhook: set".into());
        let kept = Kept {
            hook: Some(want),
            at: self.now,
        };
        self.save("app-hook.json", &kept);
    }

    /// What a look at one repo said back, in the word's: a rate limit's
    /// reset, and what wouldn't save.
    fn noted(&mut self, out: Out) {
        self.out.limited = self.out.limited.or(out.limited);
        self.out.failed.extend(out.failed);
    }
}

fn owner(repo: &str) -> &str {
    repo.split('/').next().unwrap_or(repo)
}

fn uninstalled(cfg: &Config, repo: &str) -> String {
    format!(
        "the App {} isn't installed on {}",
        cfg.app.slug,
        owner(repo)
    )
}

/// A look at a repo of the hooks', as `api`.
fn look<'a>(
    cfg: &'a Config,
    repo: &'a str,
    now: f64,
    api: &'a mut dyn Api,
    jobs: &'a mut NoJobs,
) -> Pass<'a> {
    Pass {
        cfg,
        repo,
        now,
        dry: false,
        searched: 0,
        api,
        jobs,
        out: Out::default(),
    }
}

/// The repo as GitHub has it now: Gone when it has no such repo.
fn looked_up(p: &mut Pass) -> Result<Value, Unreached> {
    let path = format!("repos/{}", p.repo);
    let r = match call_allowing(p.api, "GET", &path, None, None, &[404, 410]) {
        Ok(r) => r,
        Err(e) => {
            let e = p.noted(e);
            return Err(match e.unanswered {
                true => Unreached::Unanswered(e.why),
                false => Unreached::No(e.why),
            });
        }
    };
    if matches!(r.status, 404 | 410) {
        return Err(Unreached::Gone);
    }
    r.json().map_err(|e| Unreached::No(e.why))
}

/// The hooks start no Claude jobs.
struct NoJobs;

impl Jobs for NoJobs {
    fn state(&mut self, _: &str) -> Option<String> {
        None
    }

    fn find(&mut self, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// What a hook was set to, as the steward knows it: the first 16 hex digits
/// of the SHA-256 of `parts` as Python's json.dumps writes them, so a hook a
/// Python pass set isn't set again.
pub fn hash(parts: &Value) -> String {
    Sha256::digest(dumps(parts).as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `v` as Python's json.dumps writes it: ", " between a list's items, and
/// every character past printable ASCII escaped, as UTF-16.
fn dumps(v: &Value) -> String {
    match v {
        Value::Array(items) => {
            let items: Vec<String> = items.iter().map(dumps).collect();
            format!("[{}]", items.join(", "))
        }
        other => {
            let mut out = String::new();
            for c in other.to_string().chars() {
                if c.is_ascii() && c != '\u{7f}' {
                    out.push(c);
                } else {
                    for unit in c.encode_utf16(&mut [0; 2]) {
                        out.push_str(&format!("\\u{unit:04x}"));
                    }
                }
            }
            out
        }
    }
}

#[cfg(test)]
#[path = "hook_tests.rs"]
mod tests;
