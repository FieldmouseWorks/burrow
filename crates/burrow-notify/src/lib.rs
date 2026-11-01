//! HOST-notify: one GitHub issue per problem the owner should know about, in
//! the configured repo under its label. GitHub's app is how it reaches their
//! phone. Repeats of the same problem don't pile up: the issue gets at most
//! one comment a day, and it closes when the problem clears.
//!
//! ```text
//! HOST-notify open KEY TITLE BODY
//! HOST-notify close KEY [WHAT FIXED IT]
//! HOST-notify pending KEY
//! ```
//!
//! GitHub never fails the caller: a notice that can't be sent goes to the
//! journal and is dropped. With no repo, every notice only goes to the
//! journal. `pending` asks nothing of GitHub and answers with its exit
//! status: 0 while a notice this asked for may be open, 1 once GitHub has
//! shown it closed or it was never opened, so a caller can retry a close
//! until it has.

mod github;

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use burrow_github::{Api, Error};
use serde::Deserialize;

/// What modules/notify.nix tells it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// Its command's name (HOST-notify), which its journal lines carry.
    pub name: String,
    /// OWNER/NAME of the repo the issues go to; none for the journal only.
    pub repo: Option<String>,
    /// Who each issue is assigned to, so GitHub tells them.
    pub assignee: Option<String>,
    /// The label every notice carries: it's how it finds its own issues.
    pub label: String,
    /// One file per notice it asked for, holding when it last said so.
    pub dir: PathBuf,
    /// The user whose gh can reach GitHub, and their home.
    pub user: String,
    pub home: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// What a caller asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Ask {
    Open {
        key: String,
        title: String,
        body: String,
    },
    Close {
        key: String,
        what: String,
    },
    Pending {
        key: String,
    },
}

pub const USAGE: &str = "open KEY TITLE BODY | close KEY [WHAT] | pending KEY";

impl Ask {
    /// The words after the config. A key names a file of its own, so it's
    /// never empty, `.`, `..` or a path.
    pub fn parse(args: &[String]) -> Result<Ask, String> {
        let usage = || format!("usage: {USAGE}");
        let (op, rest) = args.split_first().ok_or_else(usage)?;
        let (key, rest) = rest.split_first().ok_or_else(usage)?;
        if key.is_empty() || key == "." || key == ".." || key.contains('/') {
            return Err(format!("{key:?} can't be a notice's key: {}", usage()));
        }
        let key = key.clone();
        let nonempty = |s: &String| !s.is_empty();
        match (op.as_str(), rest) {
            ("open", [title, body]) if nonempty(title) && nonempty(body) => Ok(Ask::Open {
                key,
                title: title.clone(),
                body: body.clone(),
            }),
            ("close", []) => Ok(Ask::Close {
                key,
                what: "Cleared.".into(),
            }),
            ("close", [what]) => Ok(Ask::Close {
                key,
                what: if what.is_empty() {
                    "Cleared.".into()
                } else {
                    what.clone()
                },
            }),
            ("pending", []) => Ok(Ask::Pending { key }),
            _ => Err(usage()),
        }
    }

    pub fn key(&self) -> &str {
        match self {
            Ask::Open { key, .. } | Ask::Close { key, .. } | Ask::Pending { key } => key,
        }
    }

    fn op(&self) -> &'static str {
        match self {
            Ask::Open { .. } => "open",
            Ask::Close { .. } => "close",
            Ask::Pending { .. } => "pending",
        }
    }
}

/// A journal line's priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Warning,
    Notice,
}

/// Where its lines go: systemd's journal.
pub trait Journal {
    fn log(&mut self, priority: Priority, line: &str);
}

/// A day: the most often an open issue hears again that its problem stands.
pub const DAY: u64 = 86_400;

/// Whether a notice this asked for may be open: its file is there.
pub fn pending(cfg: &Config, key: &str) -> bool {
    cfg.dir.join(key).exists()
}

/// Does what's asked. `now` is in seconds since the epoch. Whatever GitHub
/// does, the caller isn't failed: the journal says what went wrong.
pub fn run(cfg: &Config, ask: &Ask, api: &mut dyn Api, journal: &mut dyn Journal, now: u64) {
    let Some(repo) = cfg.repo.as_deref().filter(|r| !r.is_empty()) else {
        let words = match ask {
            Ask::Open { key, title, body } => vec![key.as_str(), title, body],
            Ask::Close { key, what } => vec![key.as_str(), what],
            Ask::Pending { key } => vec![key.as_str()],
        };
        let priority = match ask {
            Ask::Open { .. } => Priority::Warning,
            _ => Priority::Notice,
        };
        journal.log(
            priority,
            &format!("{} {} {}", cfg.name, ask.op(), words.join(" ")),
        );
        return;
    };
    let notices = Notices { cfg, repo, api };
    let done = match ask {
        Ask::Open { key, title, body } => notices.open(key, title, body, now),
        Ask::Close { key, what } => notices.close(key, what),
        Ask::Pending { .. } => Ok(()),
    };
    if let Err(e) = done {
        journal.log(
            Priority::Warning,
            &format!(
                "{} {} {}: couldn't reach GitHub ({e})",
                cfg.name,
                ask.op(),
                ask.key()
            ),
        );
    }
}

struct Notices<'a> {
    cfg: &'a Config,
    repo: &'a str,
    api: &'a mut dyn Api,
}

impl Notices<'_> {
    fn open(self, key: &str, title: &str, body: &str, now: u64) -> Result<(), Error> {
        let Notices { cfg, repo, api } = self;
        let stamp = cfg.dir.join(key);
        // Fails when GitHub can't be asked, so that's never taken for "no
        // issue", which would open a second one.
        let open = github::open_issues(api, repo, &cfg.label, key)?;
        let Some(&first) = open.first() else {
            github::label(api, repo, &cfg.label, &cfg.name);
            // Kept before it's asked for: GitHub can open the issue and the
            // answer still be lost, and then only this says it needs closing.
            write_stamp(&stamp, now)?;
            return github::create(
                api,
                repo,
                &format!("[{key}] {title}"),
                body,
                &cfg.label,
                cfg.assignee.as_deref().filter(|a| !a.is_empty()),
            );
        };
        if now.saturating_sub(said(&stamp)) > DAY {
            github::comment(api, repo, first, &format!("Still happening: {body}"))?;
            write_stamp(&stamp, now)?;
        }
        Ok(())
    }

    fn close(self, key: &str, what: &str) -> Result<(), Error> {
        let Notices { cfg, repo, api } = self;
        let stamp = cfg.dir.join(key);
        // Only what this asked for, so a clear notice asks GitHub nothing.
        if !stamp.exists() {
            return Ok(());
        }
        for n in github::open_issues(api, repo, &cfg.label, key)? {
            github::close(api, repo, n, what)?;
        }
        // Forgotten only once none is open, so a failed close is tried again
        // by the next one.
        match fs::remove_file(&stamp) {
            Err(e) if e.kind() != ErrorKind::NotFound => {
                Err(Error(format!("{}: {e}", stamp.display())))
            }
            _ => Ok(()),
        }
    }
}

/// When the notice was last said; never, when its file can't say.
fn said(stamp: &Path) -> u64 {
    fs::read_to_string(stamp)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn write_stamp(stamp: &Path, now: u64) -> Result<(), Error> {
    fs::write(stamp, format!("{now}\n")).map_err(|e| Error(format!("{}: {e}", stamp.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(w: &[&str]) -> Vec<String> {
        w.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_what_is_asked() {
        assert_eq!(
            Ask::parse(&words(&["open", "k", "Stuck", "It's stuck."])),
            Ok(Ask::Open {
                key: "k".into(),
                title: "Stuck".into(),
                body: "It's stuck.".into()
            })
        );
        for close in [&["close", "k"][..], &["close", "k", ""]] {
            assert_eq!(
                Ask::parse(&words(close)),
                Ok(Ask::Close {
                    key: "k".into(),
                    what: "Cleared.".into()
                })
            );
        }
        assert_eq!(
            Ask::parse(&words(&["close", "k", "Fixed."])),
            Ok(Ask::Close {
                key: "k".into(),
                what: "Fixed.".into()
            })
        );
        assert_eq!(
            Ask::parse(&words(&["pending", "advisory-LoK-web"])),
            Ok(Ask::Pending {
                key: "advisory-LoK-web".into()
            })
        );
        for bad in [
            &[][..],
            &["open"],
            &["open", "k", "Stuck"],
            &["open", "k", "", "body"],
            &["open", "k", "Stuck", ""],
            &["open", "k", "Stuck", "body", "more"],
            &["close", "k", "Fixed.", "more"],
            &["pending", "k", "more"],
            &["shout", "k"],
            &["open", "", "Stuck", "body"],
            &["pending", "."],
            &["pending", ".."],
            &["pending", "a/b"],
            &["pending", "/etc/passwd"],
        ] {
            assert!(Ask::parse(&words(bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn reads_only_what_it_knows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notify.json");
        let good = r#"{"name": "box-notify", "repo": "sam/homelab", "assignee": null,
                       "label": "updates", "dir": "/var/lib/burrow/notices",
                       "user": "sam", "home": "/home/sam"}"#;
        fs::write(&path, good).unwrap();
        let cfg = Config::read(&path).unwrap();
        assert_eq!(cfg.repo.as_deref(), Some("sam/homelab"));
        assert_eq!(cfg.dir, Path::new("/var/lib/burrow/notices"));
        fs::write(&path, good.replace("\"assignee\"", "\"asignee\"")).unwrap();
        assert!(Config::read(&path).unwrap_err().contains("unknown field"));
    }
}
