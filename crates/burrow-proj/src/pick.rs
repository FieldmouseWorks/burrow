//! Which waiting turn takes a slot now, and which slot ([`pick`]); and what
//! class each job's turns go as ([`class`]), which that turns on.
//!
//! The last pipelineSlots of the maxJobs slots are for pipeline turns only
//! (work on the pipeline itself, in its own repos' projects: [`class`] says),
//! so one waits only on other pipeline turns, never on everyone else's long
//! jobs; a pipeline turn takes the lowest free slot, which keeps those for the
//! next one. A lead turn (pipeline work in a lead repo's project, what the
//! pipeline is built from) is a pipeline turn that goes first of all: ahead
//! of the whole line, the other pipeline turns included. Any other turn waits
//! while its project holds maxJobsPerProject slots (pipeline turns not
//! counted), and lets those behind it by meanwhile, so one project's jobs
//! never fill the machine.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::read;

/// What a turn goes as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Pipeline work in one of the lead repos' projects: first of all.
    Lead,
    /// Pipeline work in one of the pipeline's own projects.
    Pipeline,
    /// Ahead of the line: someone's waiting on it.
    First,
    Normal,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Lead => "lead",
            Class::Pipeline => "pipeline",
            Class::First => "first",
            Class::Normal => "normal",
        }
    }

    pub fn parse(s: &str) -> Option<Class> {
        [Class::Lead, Class::Pipeline, Class::First, Class::Normal]
            .into_iter()
            .find(|c| c.name() == s)
    }

    /// The pipeline's own: its slots are theirs, and no project cap holds them.
    pub fn pipeline(self) -> bool {
        matches!(self, Class::Lead | Class::Pipeline)
    }
}

/// A slot as a waiting turn sees it.
#[derive(Clone, Debug, PartialEq)]
pub enum Slot {
    Free,
    /// Taken, by a turn of this class (None when it can't be told) and project.
    Held {
        class: Option<Class>,
        project: String,
    },
}

/// A turn waiting in line, by the name of its place in it.
#[derive(Clone, Debug, PartialEq)]
pub struct Waiter {
    pub name: String,
    pub class: Class,
    pub project: String,
}

/// The first turn in line that may start, by its index in `line`, and the
/// slot it takes (numbered from 1); None when none may.
pub fn pick(cfg: &Config, slots: &[Slot], line: &[Waiter]) -> Option<(usize, usize)> {
    let mut held: HashMap<&str, usize> = HashMap::new();
    let mut free = Vec::new();
    for (i, slot) in slots.iter().enumerate() {
        match slot {
            Slot::Free => free.push(i + 1),
            Slot::Held { class, project } => {
                if !class.is_some_and(Class::pipeline) {
                    *held.entry(project).or_default() += 1;
                }
            }
        }
    }
    let lead = line
        .iter()
        .enumerate()
        .filter(|(_, w)| w.class == Class::Lead);
    let rest = line
        .iter()
        .enumerate()
        .filter(|(_, w)| w.class != Class::Lead);
    for (i, w) in lead.chain(rest) {
        if !w.class.pipeline()
            && held.get(w.project.as_str()).copied().unwrap_or(0) >= cfg.max_jobs_per_project
        {
            continue;
        }
        if let Some(&n) = free
            .iter()
            .find(|&&n| w.class.pipeline() || n <= cfg.max_jobs.saturating_sub(cfg.pipeline_slots))
        {
            return Some((i, n));
        }
    }
    None
}

/// A clone's origin URL, or nothing (no clone, no origin, or no path at all:
/// git -C "" would ask whatever repo it runs in).
pub fn origin(cfg: &Config, path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    Command::new(&cfg.tools.git)
        .args(["-C", path, "remote", "get-url", "origin"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim_end_matches('\n')
                .to_owned()
        })
        .unwrap_or_default()
}

/// The GitHub repo a clone is of, as owner/name in lower case: [`github_name`]
/// of its origin.
pub fn github_repo(cfg: &Config, path: &str) -> String {
    github_name(&origin(cfg, path)).unwrap_or_default()
}

/// The GitHub repo an origin is of, as owner/name in lower case, in any form
/// git takes (https, ssh:// or scp-like, a user or a .git or not), or None
/// for anything else: another host, a local path, a longer path, a query, or
/// a name GitHub wouldn't give (so what it gives is never more than a repo's
/// name, whatever else the origin holds).
pub fn github_name(url: &str) -> Option<String> {
    let url = url.to_lowercase();
    let url = url.strip_suffix('/').unwrap_or(&url);
    let url = url.strip_suffix(".git").unwrap_or(url);
    // An optional user, which holds neither @ nor /.
    let user = |s: &str| -> String {
        match s.split_once('@') {
            Some((u, rest)) if !u.is_empty() && !u.contains('/') => rest.to_owned(),
            _ => s.to_owned(),
        }
    };
    let rest = if let Some(r) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    {
        user(r).strip_prefix("github.com/")?.to_owned()
    } else if let Some(r) = url.strip_prefix("ssh://") {
        let host = user(r);
        let r = host.strip_prefix("github.com")?;
        let r = match r.strip_prefix(':') {
            Some(p) => {
                let (port, path) = p.split_at(p.find('/')?);
                if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                    return None;
                }
                path
            }
            None => r,
        };
        r.strip_prefix('/')?.to_owned()
    } else {
        user(url).strip_prefix("github.com:")?.to_owned()
    };
    let (owner, name) = rest.split_once('/')?;
    let ok = |s: &str, extra: &[u8]| {
        !s.is_empty()
            && s.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || extra.contains(&b)
            })
    };
    (ok(owner, b"") && ok(name, b"._")).then_some(rest)
}

/// Whether a repo ([`github_repo`]'s owner/name) is in a list of
/// OWNER/NAMEs, any case.
pub fn listed(repo: &str, list: &[String]) -> bool {
    !repo.is_empty() && list.iter().any(|r| r.to_lowercase() == repo)
}

/// Whether the project at a path is the pipeline's own: the machine's flake,
/// which deploys everything, wherever its origin is; or a clone of one of the
/// pipeline's own repos. The flake is the same directory however either path
/// spells it (doubled or trailing slashes, dots, symlinks); a path that's
/// empty or isn't there is never it.
pub fn pipeline_project(cfg: &Config, path: &str) -> bool {
    if let (Ok(p), Ok(f)) = (fs::canonicalize(path), fs::canonicalize(&cfg.flake))
        && p == f
    {
        return true;
    }
    listed(&github_repo(cfg, path), &cfg.pipeline_repos)
}

/// The project a job runs in, as its dir has it; "" when it can't be read.
pub fn project(dir: &Path) -> String {
    read(&dir.join("project")).unwrap_or_default()
}

/// A job's class, from its dir. One run with --pipeline is a pipeline one,
/// every turn of it, only while its project is one of the pipeline's own;
/// anywhere else it's first, every turn of it, so it still goes ahead of the
/// line but counts against its project's cap and stays off the pipeline's
/// slots (one repo's PRs, waiting on that repo's own lock, can't fill the
/// machine). In a lead repo's project it's lead. Asked each time, so a job
/// whose repo was taken off a list since is held to it too. Otherwise first
/// or normal, as the turn was started (`given`).
pub fn class(cfg: &Config, dir: &Path, given: Class) -> Class {
    if !dir.join("pipeline").exists() {
        return given;
    }
    let d = project(dir);
    if listed(&github_repo(cfg, &d), &cfg.lead_repos) {
        Class::Lead
    } else if pipeline_project(cfg, &d) {
        Class::Pipeline
    } else {
        Class::First
    }
}

/// A waiting turn's class now, and the name its place in line
/// (PREFIX-WHEN-ID) should have for it: 0 lead or pipeline ([`pick`] takes
/// lead ones first), 1 first, 2 normal. Its prefix says what the turn itself
/// is (2 normal, anything else first: run with --first, or waited past
/// maxWait), and its class what that makes it now, so a job whose origin or
/// list changed while it waits moves in line and is held to its project's
/// cap as what it is now.
pub fn place(cfg: &Config, name: &str, dir: &Path) -> (Class, String) {
    let given = if name.starts_with("2-") {
        Class::Normal
    } else {
        Class::First
    };
    let c = class(cfg, dir, given);
    let p = match c {
        Class::Lead | Class::Pipeline => 0,
        Class::First => 1,
        Class::Normal => 2,
    };
    let mut chars = name.chars();
    chars.next();
    let rest = chars.as_str().strip_prefix('-').unwrap_or(name);
    (c, format!("{p}-{rest}"))
}

/// What proj says of a job that asked to be pipeline work and isn't, so a
/// mis-set origin or list doesn't quietly demote it: None for any other.
/// Never the origin itself, which can hold a token: only the repo it names.
pub fn demoted(cfg: &Config, dir: &Path) -> Option<String> {
    if !dir.join("pipeline").exists() {
        return None;
    }
    let d = project(dir);
    if pipeline_project(cfg, &d) {
        return None;
    }
    let r = github_repo(cfg, &d);
    Some(if !r.is_empty() {
        format!(
            "asked as --pipeline, but its origin, github.com/{r}, isn't in pipelineRepos: it goes as --first"
        )
    } else if !origin(cfg, &d).is_empty() {
        "asked as --pipeline, but its origin isn't a GitHub repo's: it goes as --first".to_owned()
    } else {
        "asked as --pipeline, but its project has no origin: it goes as --first".to_owned()
    })
}

#[cfg(test)]
mod tests;
