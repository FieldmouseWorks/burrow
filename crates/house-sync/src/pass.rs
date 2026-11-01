//! One pass: every repo the owners keep, or the one a push names, gets one
//! PR wherever its house blocks differ from burrow's, from an ordinary
//! branch, so the steward reviews and lands it like any other PR. The PRs
//! for older blocks are closed, so none of them lands after the current
//! one. Broken markers and repos it couldn't get through go to the owner,
//! but only from a full pass: one repo's pass says so to the journal, and
//! the next full pass puts it in the notice.

use std::fs::File;
use std::io::Write;

use burrow_github::{Api, Error};

use crate::blocks::{House, Rewritten, rewrite, same_tree};
use crate::config::Config;
use crate::github::{self, Open, Pr, Repo};

/// How the owner hears: HOST-notify's `open KEY TITLE BODY` and
/// `close KEY TITLE`.
pub trait Notify {
    fn notify(&mut self, args: &[&str]) -> Result<(), String>;
}

/// What a pass was asked to do.
pub struct Ask<'a> {
    /// Only say what it would do, changing nothing.
    pub dry: bool,
    /// One repo's pass (a push to its default branch), else every repo's.
    pub only: Option<&'a str>,
}

/// Runs a pass. Ok(false) when one repo's pass couldn't get through its
/// repo; Err when the pass couldn't run at all, or couldn't tell the owner.
pub fn run(
    cfg: &Config,
    ask: &Ask,
    api: &mut dyn Api,
    notify: &mut dyn Notify,
    out: &mut dyn Write,
) -> Result<bool, String> {
    let dry = ask.dry || cfg.dry_run;
    // One pass at a time, and only for the blocks the system has now: a
    // pass from before a switch, still running or still waiting here, would
    // close the PR for the new blocks as one for older ones. A pass that
    // waited for the lock checks the blocks after it, when it would start.
    let _lock = if dry { None } else { Some(lock(cfg)?) };
    if !same_tree(&cfg.house, &cfg.current) {
        let _ = writeln!(
            out,
            "house-sync: the system has other house blocks than this pass; the pass for those does the work"
        );
        return Ok(true);
    }
    let mut pass = Pass {
        house: House::read(&cfg.house).map_err(|e| format!("{}: {e}", cfg.house.display()))?,
        dry,
        api,
        out,
        failed: Vec::new(),
        mangled: Vec::new(),
    };

    let mut repos = Vec::new();
    if let Some(only) = ask.only {
        // One repo: only one of the owners', as a full pass would list it.
        match github::repo(pass.api, only, &cfg.owners) {
            Ok(Some(repo)) => repos.push(repo),
            Ok(None) => pass.say(&format!("{only}: not a repo it keeps")),
            Err(e) => {
                pass.say(&format!(
                    "{only}: couldn't read it, trying again next pass ({e})"
                ));
                return Ok(false);
            }
        }
    } else {
        for owner in &cfg.owners {
            // An owner whose list can't be read is a failure too; the
            // others go on.
            match github::repos(pass.api, owner) {
                Ok(found) => repos.extend(found),
                Err(e) => {
                    pass.say(&format!(
                        "{owner}: couldn't list its repos, trying again next pass ({e})"
                    ));
                    pass.failed.push(format!("{owner} (its repo list)"));
                }
            }
        }
    }
    for repo in repos {
        if cfg.exclude.iter().any(|e| e.matches(&repo.name)) {
            continue;
        }
        if let Err(e) = pass.repo(&repo) {
            pass.say(&format!(
                "{}: failed, trying again next pass ({e})",
                repo.name
            ));
            pass.failed.push(repo.name);
        }
    }

    if dry {
        return Ok(true);
    }
    // The owner's notices are about every repo, so only a full pass changes
    // them.
    if ask.only.is_some() {
        return Ok(pass.failed.is_empty());
    }
    let mut tell = |args: &[&str]| notify.notify(args);
    if !pass.mangled.is_empty() {
        tell(&[
            "open",
            "house-markers",
            "The house blocks' markers are broken",
            &format!(
                "house-sync can't find one whole copy of each house block in these repos' AGENTS.md, so it can't keep them current: {}. Put back one begin and one end marker line around each block, as burrow's house/ has them (the journal names the block).",
                pass.mangled.join(", ")
            ),
        ])?;
    } else if pass.failed.is_empty() {
        // Only once every repo was read: one that failed may still be
        // broken.
        tell(&[
            "close",
            "house-markers",
            "Every house block is readable again.",
        ])?;
    }
    if pass.failed.is_empty() {
        tell(&[
            "close",
            "house-sync",
            "house-sync got through every repo again.",
        ])?;
    } else {
        tell(&[
            "open",
            "house-sync",
            "house-sync failed on some repos",
            &format!(
                "These couldn't be checked or updated on the last pass (see journalctl -u house-sync): {}. It tries again on the next pass: a push to the repo's default branch, or the hourly catch-up.",
                pass.failed.join(", ")
            ),
        ])?;
    }
    Ok(true)
}

/// The lock every pass that can change anything holds, waited for.
fn lock(cfg: &Config) -> Result<File, String> {
    let why = |e: std::io::Error| format!("{}: {e}", cfg.lock.display());
    let file = File::options()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&cfg.lock)
        .map_err(why)?;
    file.lock().map_err(why)?;
    Ok(file)
}

struct Pass<'a> {
    house: House,
    dry: bool,
    api: &'a mut dyn Api,
    out: &'a mut dyn Write,
    failed: Vec<String>,
    mangled: Vec<String>,
}

impl Pass<'_> {
    fn say(&mut self, what: &str) {
        // The journal; a pass goes on whether or not it takes the line.
        let _ = writeln!(self.out, "house-sync: {what}");
    }

    fn repo(&mut self, repo: &Repo) -> Result<(), Error> {
        let name = repo.name.as_str();
        // One snapshot of the default branch: the file is read at this
        // commit and the branch starts from it, so the PR can't undo a
        // change made meanwhile.
        let sha = github::head(self.api, name, &repo.base)?;
        // No AGENTS.md yet: the baseline job writes it, blocks included.
        let Some((file_sha, agents)) = github::agents(self.api, name, &sha)? else {
            return Ok(());
        };
        let text = match rewrite(&agents, &self.house) {
            Rewritten::NoBlock => {
                self.say(&format!(
                    "{name}: no house block yet (its baseline job or the rollout adds it)"
                ));
                return Ok(());
            }
            Rewritten::Mangled(block) => {
                self.say(&format!(
                    "{name}: the markers of its house:{block} block are mangled"
                ));
                self.mangled.push(name.into());
                return Ok(());
            }
            Rewritten::Text(text) => text,
        };
        let open = github::open_prs(self.api, name)?;
        let rev = self.house.rev.clone();
        if text == agents {
            let why = format!(
                "AGENTS.md on {} already has the current house blocks ({rev}).",
                repo.base
            );
            return self.close_older(name, &open, &why);
        }
        // The repo that holds house/ itself: a merge there reaches its default
        // branch before this system has it, and a PR now would take those
        // blocks back out of its AGENTS.md. The pass after the switch has them.
        for block in &self.house.blocks {
            let source = format!("house/{}.md", block.name);
            if github::file(self.api, name, &source, &sha)?.is_some_and(|text| text != block.text) {
                self.say(&format!(
                    "{name}: its own {source} isn't this system's; the pass for that one does the work"
                ));
                return Ok(());
            }
        }
        // Already open for these blocks: only the cleanup may be left to do.
        let branch = self.house.branch();
        if let Some(current) = open.iter().find(|p| p.head == branch) {
            return self.close_older(name, &open, &format!("Replaced by {}.", current.url));
        }
        if self.dry {
            self.say(&format!(
                "{name}: would open a PR updating its house blocks to {rev}"
            ));
            return self.close_older(name, &open, &format!("Replaced by the PR for {rev}."));
        }
        let url = github::open(
            self.api,
            &Open {
                repo: name,
                base: &repo.base,
                sha: &sha,
                branch: &branch,
                file_sha: &file_sha,
                text: &text,
                rev: &rev,
            },
        )?;
        self.say(&format!("{name}: opened {url}"));
        self.close_older(name, &open, &format!("Replaced by {url}."))
    }

    /// Closes the open PRs for older blocks. Any close that fails fails the
    /// repo, after the rest are tried, and the next pass tries it again.
    fn close_older(&mut self, repo: &str, open: &[Pr], why: &str) -> Result<(), Error> {
        let branch = self.house.branch();
        let mut failed = None;
        for pr in open
            .iter()
            .filter(|p| p.head.starts_with("house/rules-") && p.head != branch)
        {
            if self.dry {
                self.say(&format!("{repo}: would close #{} ({why})", pr.number));
            } else if let Err(e) = github::close(self.api, repo, pr, why) {
                failed = Some(e);
            }
        }
        failed.map_or(Ok(()), Err)
    }
}
