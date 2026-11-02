//! One run of HOST-update: take origin/main, bump every flake input and
//! Claude Code, build, run the VM tests, put it live with HOST-switch, check
//! the box is still healthy, HOST-confirm, push to main. Anything that
//! breaks goes back by itself and is handed to a Claude job to fix (acting
//! on GitHub as the machine's own App, when it has one); the owner hears
//! about it only if that doesn't work. It also deploys main itself, so a
//! merged PR goes live the same way.
//!
//! It works in its own clone (`work`), never in the operator's checkout, and
//! holds off while a switch is pending or while the box runs changes that
//! aren't on main yet. In a repo whose PRs land through its merge train,
//! what it would push goes up as a PR instead (train.rs), and it rides that
//! PR until it lands, or puts a fresh bump on it when a check fails on it.
//!
//! Its steps, in order: this file's, to the build; vm_tests.rs, the VM
//! tests; live.rs, live and confirmed; and this file's again, put up.

use std::fs::{self, File};
use std::path::Path;
use std::process::Command;

use burrow_github::Api;
use rustix::fs::{FlockOperation, flock};

use crate::bump::{self, CLAUDE};
use crate::dispatch::{dispatch, open, settled};
use crate::memo::Memo;
use crate::running::{self, Already, Running};
use crate::status::{Outcome, epoch, note};
use crate::train::{self, Pr};
use crate::{Broke, Config, is_name, push};

/// How long a run holds off for a system that isn't main's before the
/// owner hears of it.
const HELD_FOR: i64 = 6 * 3600;

/// Why a run stops short: it's said what it did (`note`) and exits so, or
/// something it needed broke.
pub(crate) enum End {
    Exit(u8),
    Broke(Broke),
}

impl From<Broke> for End {
    fn from(b: Broke) -> End {
        End::Broke(b)
    }
}

pub(crate) type Step = Result<(), End>;

/// A run, as it goes: what it found, and what it's done so far.
pub struct Run<'a> {
    pub(crate) cfg: &'a Config,
    pub(crate) api: &'a mut dyn Api,
    pub(crate) memo: Memo<'a>,
    /// The VM tests it runs.
    pub(crate) checks: Vec<String>,
    /// main as it fetched it.
    pub(crate) main: String,
    /// What the machine ran when it began.
    pub(crate) running: Running,
    pub(crate) inputs: Vec<String>,
    /// Its PR, in a train repo, when it rides it, and when its head is red
    /// and a fresh bump may replace it.
    pub(crate) riding: Option<u64>,
    pub(crate) replacing: Option<Pr>,
    /// Its open PR's number, while it holds off for that to land.
    pub(crate) behind: Option<u64>,
    /// HEAD is a commit of this run's: a bump, or the merge that rides its
    /// PR.
    pub(crate) changed: bool,
    /// What it couldn't fetch, and what didn't build.
    pub(crate) unfetched: Vec<String>,
    pub(crate) failed: Vec<String>,
    /// HEAD's system.
    pub(crate) new: String,
    /// It ran already: put up, not switched.
    pub(crate) live: bool,
    /// The VM tests it went live without, and the serious vulnerabilities
    /// it took away that let it.
    pub(crate) untested: Vec<String>,
    pub(crate) fixes: Vec<String>,
    /// Its PR's URL, once it's up.
    pub(crate) url: Option<String>,
    /// What every fix job is told about working on the flake.
    pub(crate) rules: String,
}

/// One run, unless another is going: its exit, with what it did said and
/// kept in status.json. `force` runs the whole loop even when nothing
/// upstream moved, and forgets what failed before.
pub fn run(cfg: &Config, api: &mut dyn Api, force: bool) -> u8 {
    for dir in ["jobs", "logs"] {
        if let Err(e) = fs::create_dir_all(cfg.state.join(dir)) {
            eprintln!("{}: {}: {e}", cfg.update, cfg.state.join(dir).display());
            return 1;
        }
    }
    let held = match File::create(&cfg.lock) {
        Ok(lock) => lock,
        Err(e) => {
            eprintln!("{}: {}: {e}", cfg.update, cfg.lock.display());
            return 1;
        }
    };
    if flock(&held, FlockOperation::NonBlockingLockExclusive).is_err() {
        println!("another {} is running", cfg.update);
        return 0;
    }
    let ended = once(cfg, api, force);
    drop(held);
    ended
}

/// One run, which nothing else is: the lock is `run`'s. (In a process of
/// many threads, as the tests are, a child another thread is starting holds
/// a copy of the lock until it execs: a run straight after another can
/// find it held.)
pub(crate) fn once(cfg: &Config, api: &mut dyn Api, force: bool) -> u8 {
    let checks = match std::env::var("BURROW_UPDATE_CHECKS") {
        Ok(c) if !c.is_empty() => c.split_whitespace().map(String::from).collect(),
        _ => cfg.checks.clone(),
    };
    // Each goes into a flake reference.
    if let Some(c) = checks.iter().find(|c| !is_name(c)) {
        eprintln!("{}: {c}: not a VM test's name", cfg.update);
        return 2;
    }
    let mut r = Run {
        rules: rules(cfg, &checks),
        cfg,
        api,
        memo: Memo { cfg, force },
        checks,
        main: String::new(),
        running: Running {
            current: String::new(),
            rev: None,
        },
        inputs: Vec::new(),
        riding: None,
        replacing: None,
        behind: None,
        changed: false,
        unfetched: Vec::new(),
        failed: Vec::new(),
        new: String::new(),
        live: false,
        untested: Vec::new(),
        fixes: Vec::new(),
        url: None,
    };
    match r.steps() {
        Ok(()) => 0,
        Err(End::Exit(code)) => code,
        Err(End::Broke(Broke(why))) => {
            eprintln!("{}: {why}", cfg.update);
            1
        }
    }
}

/// What every fix job is told about working on the flake.
fn rules(cfg: &Config, checks: &[String]) -> String {
    let to_main = if cfg.train.is_some() {
        "commit, push it to a branch of your own and open a PR against main, which the steward settles and the merge train lands (nothing else may move main)"
    } else {
        "commit and push to main"
    };
    let checks: Vec<String> = checks
        .iter()
        .map(|c| format!(".#checks.{}.{c}", cfg.system))
        .collect();
    let flake = cfg.flake.display();
    format!(
        "Work in your own worktree (git -C {flake} worktree add \"$PROJ_SCRATCH/fix\" origin/main), never in {flake} itself: that checkout is the operator's. When the toplevel builds and `nix build -L {}` passes there, {to_main}; {} deploys main on its next run, with the dead man's switch. If the right answer is to wait for upstream, say so and change nothing. If it needs {}, say exactly what.",
        checks.join(" "),
        cfg.update,
        cfg.owner
    )
}

impl Run<'_> {
    fn steps(&mut self) -> Step {
        self.begin()?;
        self.bump()?;
        self.build()?;
        self.replaces()?;
        self.already()?;
        self.vm_tests()?;
        self.amend()?;
        self.go_live()?;
        self.put_up()
    }

    /// Says what the run did, and ends it: 1 when it failed, 0 otherwise.
    pub(crate) fn end(&self, outcome: Outcome, message: &str) -> End {
        note(self.cfg, outcome, message, self.behind);
        End::Exit(if outcome == Outcome::Failed { 1 } else { 0 })
    }

    pub(crate) fn git(&self, args: &[&str]) -> Result<String, Broke> {
        self.cfg.git(args).map_err(Broke)
    }

    /// What HEAD brings, as the run says it: what moved since main, or
    /// main itself, when that's what HEAD is (a merged PR to put live).
    pub(crate) fn what(&self) -> Result<String, Broke> {
        if self.changed {
            Ok(bump::said(&bump::describe(
                self.cfg,
                &self.inputs,
                &self.main,
            )))
        } else {
            Ok(format!(
                "main {}",
                self.git(&["rev-parse", "--short", "HEAD"])?
            ))
        }
    }

    pub(crate) fn dispatch(&self, key: &str, prompt: &str) {
        if let Some(id) = dispatch(self.cfg, key, prompt) {
            println!("handed to Claude: proj log {id}");
        }
    }

    /// Never fighting a human (a pending switch is someone mid-change), a
    /// clean copy of main, and whether the box runs main, or something main
    /// has since moved past: then it's this run's to update. If it runs
    /// changes that aren't on main, someone is mid-work: it holds. In a
    /// train repo, its PR, when it's open: main with that on top, and
    /// nothing fresh, unless it's red on a head of its own, which a fresh
    /// bump replaces.
    fn begin(&mut self) -> Step {
        let cfg = self.cfg;
        if cfg.switch_state.join("pending").exists() {
            let switch = name(&cfg.tools.switch);
            return Err(self.end(
                Outcome::Held,
                &format!("a {switch} is pending; trying again next run"),
            ));
        }

        if !cfg.work.join(".git").is_dir() {
            let parent = cfg.work.parent().unwrap_or(Path::new("/"));
            let made = Command::new(&cfg.tools.install)
                .args(["-d", "-o", &cfg.user, "-g", &cfg.group])
                .arg(parent)
                .status()
                .is_ok_and(|s| s.success());
            if !made {
                return Err(Broke(format!("{}: couldn't be made", parent.display())).into());
            }
            let cloned = cfg
                .run_as_user("git")
                .args(["clone", "-q", &cfg.origin])
                .arg(&cfg.work)
                .status()
                .is_ok_and(|s| s.success());
            if !cloned {
                return Err(Broke(format!("git clone {}: failed", cfg.origin)).into());
            }
        }
        self.git(&["fetch", "-q", "origin", "main"])?;
        self.git(&["checkout", "-q", "-B", "update", "origin/main"])?;
        self.git(&["reset", "-q", "--hard", "origin/main"])?;
        self.git(&["clean", "-qfdx"])?;
        self.main = self.git(&["rev-parse", "HEAD"])?;

        let current = crate::status::running(cfg);
        let rev = Command::new(cfg.current_system.join("sw/bin/nixos-version"))
            .arg("--configuration-revision")
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim_end_matches('\n')
                    .to_string()
            })
            .unwrap_or_default();
        self.running = Running::parse(&current, &rev).unwrap_or(Running { current, rev: None });
        let run = &self.running;
        let in_sync = run.rev.as_ref().is_some_and(|rev| {
            !rev.ends_with("-dirty")
                && cfg
                    .git_hushed(&["merge-base", "--is-ancestor", rev, &self.main])
                    .is_ok()
        }) || run.current == cfg.recorded("deployed")
            || running::same_as_running(cfg, run);
        if !in_sync {
            let since = cfg.state.join("held-since");
            if !since.exists() {
                let _ = fs::write(&since, format!("{}\n", epoch()));
            }
            let held = cfg.recorded("held-since").parse::<i64>().unwrap_or(epoch());
            if epoch() - held > HELD_FOR {
                open(
                    cfg,
                    "not-on-main",
                    &format!("{} runs changes that aren't on main", cfg.host),
                    &format!(
                        "For over 6 hours {} has been running a system that isn't built from {} main ({}, revision {}), so automatic updates are on hold. Ask the operator to commit and push what it switched to (or to switch back to main).",
                        cfg.host,
                        cfg.origin,
                        run.current,
                        run.rev.as_deref().unwrap_or("unknown")
                    ),
                );
            }
            return Err(self.end(
                Outcome::Held,
                &format!(
                    "{} runs changes that aren't on main; holding (see {} status)",
                    cfg.host, cfg.update
                ),
            ));
        }
        let _ = fs::remove_file(cfg.state.join("held-since"));
        settled(
            cfg,
            "not-on-main",
            &format!("{} is back on main.", cfg.host),
        );

        let Some(t) = &cfg.train else { return Ok(()) };
        let pr = match train::open(t, self.api) {
            Ok(pr) => pr,
            Err(e) => {
                eprintln!("{e}");
                return Err(self.end(
                    Outcome::Held,
                    "GitHub didn't say whether its PR is open; trying again next run",
                ));
            }
        };
        let Some(pr) = pr else { return Ok(()) };
        match train::red(cfg, t, self.api, &pr) {
            Err(why) => Err(self.end(Outcome::Held, &format!("{why}; trying again next run"))),
            Ok(true) => {
                self.replacing = Some(pr);
                Ok(())
            }
            Ok(false) => {
                self.behind = Some(pr.number);
                if let Err(why) = train::ride(cfg, t, &pr) {
                    return Err(self.end(Outcome::Held, &why));
                }
                self.riding = Some(pr.number);
                Ok(())
            }
        }
    }

    /// What's new upstream: every flake input, and Claude Code's latest
    /// release. Riding its PR, nothing: what's new is what the PR has. With
    /// nothing new, main running and no force, it's up to date.
    fn bump(&mut self) -> Step {
        let cfg = self.cfg;
        self.inputs = bump::inputs(cfg)?;
        fs::write(cfg.state.join("logs/update.log"), "")
            .map_err(|e| Broke(format!("logs/update.log: {e}")))?;
        if self.riding.is_some() {
            self.changed = self.git(&["rev-parse", "HEAD"])? != self.main;
        } else {
            for i in self.inputs.iter().map(String::as_str).chain([CLAUDE]) {
                if !bump::update_one(cfg, i) {
                    self.unfetched.push(i.to_string());
                }
            }
            self.changed = bump::commit(cfg, &self.inputs);
        }
        if !self.changed && !self.memo.force && running::main_runs(cfg, &self.running, &self.main) {
            running::live_at(cfg, &self.main, &self.running.current).map_err(Broke)?;
            return Err(self.end(Outcome::Current, "up to date"));
        }
        Ok(())
    }

    /// The build. If everything at once doesn't, it takes what does, one
    /// input at a time, so one broken input never holds back another's
    /// security fix. Riding, main with its PR on top builds or the PR's
    /// branch gets a fix.
    fn build(&mut self) -> Step {
        let cfg = self.cfg;
        let logs = cfg.state.join("logs");
        if let Some(n) = self.riding {
            match self.built() {
                Some(new) => self.new = new,
                None => {
                    if !self.memo.failed_before("build") {
                        self.memo.failed("build")?;
                    }
                    let e = self.end(
                        Outcome::Failed,
                        &format!("main with its PR #{n} on top doesn't build"),
                    );
                    self.dispatch(
                        "build-riding",
                        &format!(
                            "{update}: {origin} main ({main}) with {update}'s own PR #{n} merged on top doesn't build; the log is {log}. Fix it on that PR's branch, in your own worktree, so the train can land it; {update} holds off until it has. Never push to main.",
                            update = cfg.update,
                            origin = cfg.origin,
                            main = self.main,
                            log = logs.join("build.log").display(),
                        ),
                    );
                    return Err(e);
                }
            }
        }
        settled(cfg, "build-riding", "main with its PR on top builds.");
        if self.riding.is_none() {
            match self.built() {
                Some(new) => self.new = new,
                None => self.input_by_input()?,
            }
        }
        if self.failed.is_empty() {
            settled(cfg, "build-inputs", "Cleared by a later run.");
        } else {
            self.dispatch(
                "build-inputs",
                &format!(
                    "{}: these updates don't build with {}'s flake: {}. The logs are {}/build-<input>.log. Everything else went through without them. Make them build. {}",
                    cfg.update,
                    cfg.host,
                    self.failed.join(" "),
                    logs.display(),
                    self.rules
                ),
            );
        }
        settled(cfg, "build-main", "main builds again.");
        Ok(())
    }

    /// HEAD's system, unless its tree failed to build in the last day.
    fn built(&self) -> Option<String> {
        if self.memo.failed_before("build") {
            return None;
        }
        running::build(self.cfg)
    }

    /// Everything at once didn't build: each input on its own, on main,
    /// kept when it builds. Main itself not building is main's to fix.
    fn input_by_input(&mut self) -> Step {
        let cfg = self.cfg;
        let logs = cfg.state.join("logs");
        if !self.memo.failed_before("build") {
            copy(&logs.join("build.log"), &logs.join("build-all.log"));
            self.memo.failed("build")?;
        }
        self.git(&["reset", "-q", "--hard", &self.main])?;
        self.unfetched.clear();
        let inputs = self.inputs.clone();
        for i in inputs.iter().map(String::as_str).chain([CLAUDE]) {
            if !bump::update_one(cfg, i) {
                self.unfetched.push(i.to_string());
            }
            if !bump::commit(cfg, &self.inputs) {
                continue;
            }
            match self.built() {
                Some(new) => {
                    self.new = new;
                    println!("{i}: builds");
                }
                None => {
                    if !self.memo.failed_before("build") {
                        copy(
                            &logs.join("build.log"),
                            &logs.join(format!("build-{i}.log")),
                        );
                        self.memo.failed("build")?;
                    }
                    self.git(&["reset", "-q", "--hard", "HEAD~1"])?;
                    self.failed.push(i.to_string());
                }
            }
        }
        // None of them built: main itself, with nothing new on it.
        if self.git(&["rev-parse", "HEAD"])? == self.main {
            self.changed = false;
        }
        match running::build(cfg) {
            Some(new) => {
                self.new = new;
                Ok(())
            }
            None => {
                let e = self.end(Outcome::Failed, "main itself doesn't build");
                self.dispatch(
                    "build-main",
                    &format!(
                        "{}: {} main ({}) doesn't build; the log is {}. Fix it. {}",
                        cfg.update,
                        cfg.origin,
                        self.main,
                        logs.join("build.log").display(),
                        self.rules
                    ),
                );
                open(
                    cfg,
                    "build-main",
                    "main doesn't build",
                    &format!(
                        "{} can't build {} main ({}). A Claude job is on it; this closes when main builds again.",
                        cfg.update, cfg.origin, self.main
                    ),
                );
                Err(e)
            }
        }
    }

    /// Its red PR's head stays, unless a whole fresh bump other than it
    /// replaces it.
    fn replaces(&mut self) -> Step {
        let Some(pr) = &self.replacing else {
            return Ok(());
        };
        let short: Vec<String> = self.unfetched.iter().chain(&self.failed).cloned().collect();
        match train::replaces(self.cfg, pr, &self.main, &short) {
            Ok(()) => Ok(()),
            Err(why) => Err(self.end(
                Outcome::Held,
                &format!(
                    "its PR #{} is red, and {why}; holding for the Claude jobs on it",
                    pr.number
                ),
            )),
        }
    }

    /// Running already, and not on main: what a run before put live (its
    /// PR didn't open, or closed, or the train took it while that run was
    /// testing a newer one, or its push to main was refused), or a bump
    /// that changes only what the checks use. It goes up with what any
    /// other bump has: the switch and the health check it passed when it
    /// went live, and the VM tests, on this tree (which isn't the one that
    /// went live once main moved under it or the checks' inputs did).
    /// Riding, it's what its PR put live, with nothing since that changes
    /// the system: nothing to do until the PR lands. Nothing is switched
    /// either way: the box keeps the system it runs, under an older
    /// commit's stamp.
    fn already(&mut self) -> Step {
        let cfg = self.cfg;
        let already = running::runs_already(
            cfg,
            &self.running,
            &self.new,
            self.changed,
            self.riding.is_some(),
        );
        match already {
            None => Ok(()),
            Some(Already::Up) => {
                self.new = self.running.current.clone();
                self.live = true;
                Ok(())
            }
            Some(Already::Hold) => {
                let what = self.what()?;
                Err(self.end(
                    Outcome::Current,
                    &format!(
                        "{} runs what {what} builds, but {} didn't put it live, so it never had the health check; holding it until main has it",
                        cfg.host, cfg.update
                    ),
                ))
            }
            Some(Already::Current) => {
                running::live_at(cfg, &self.main, &self.running.current).map_err(Broke)?;
                Err(self.end(
                    Outcome::Current,
                    &format!(
                        "nothing new builds; up to date{}{}",
                        self.riding_said(),
                        self.held_back()
                    ),
                ))
            }
        }
    }

    pub(crate) fn riding_said(&self) -> String {
        self.riding
            .map(|n| format!(" (riding #{n})"))
            .unwrap_or_default()
    }

    pub(crate) fn held_back(&self) -> String {
        if self.failed.is_empty() {
            String::new()
        } else {
            format!(" (held back: {})", self.failed.join(" "))
        }
    }

    /// The commit says what was skipped, before it can go up.
    fn amend(&mut self) -> Step {
        if self.untested.is_empty() || !self.changed || self.riding.is_some() {
            return Ok(());
        }
        let what = self.what()?;
        self.git(&[
            "commit",
            "-q",
            "--amend",
            "-m",
            &format!("deps: update {what}"),
            "-m",
            &format!(
                "Pushed by {} without the VM test {}, which fails on main itself, because it fixes:\n{}\nIt built and stayed healthy live.",
                self.cfg.update,
                self.untested.join(" "),
                self.fixes.join("\n")
            ),
        ])?;
        Ok(())
    }

    /// Put up, and the operator's checkout brought along when it isn't
    /// mid-work. (The running system keeps the old commit as its revision;
    /// `deployed` is how the next run knows it's this one.)
    fn put_up(&mut self) -> Step {
        let cfg = self.cfg;
        fs::write(cfg.state.join("deployed"), format!("{}\n", self.new))
            .map_err(|e| Broke(format!("deployed: {e}")))?;
        // What went live, in words: pushing can rebase it onto what hasn't.
        let went = self.what()?;
        if let Some(t) = &cfg.train {
            if self.riding.is_some() || !self.changed {
                // Riding, or nothing new on main: nothing to put up.
            } else if let Some(pr) = &self.replacing {
                // Up already, before its confirm, unless it was live before
                // this run: then it goes up now, and a refusal leaves the
                // box as this run found it.
                if self.live {
                    match train::replace(cfg, t, self.api, pr) {
                        Ok(url) => self.url = Some(url),
                        Err(why) => println!("{why}"),
                    }
                }
            } else {
                match train::propose(cfg, t, self.api) {
                    Ok(url) => self.url = Some(url),
                    Err(why) => {
                        eprintln!("{why}");
                        println!("its PR didn't open; the next run opens it");
                    }
                }
            }
            if !self.changed {
                let head = self.git(&["rev-parse", "HEAD"])?;
                fs::write(cfg.state.join("deployed-main"), format!("{head}\n"))
                    .map_err(|e| Broke(format!("deployed-main: {e}")))?;
            }
        } else if let Err(why) = push::push_main(cfg) {
            println!("{why}");
        }
        self.bring_operator_along();
        let through = running::live_through(cfg, &self.main);
        running::live_at(cfg, &through, &self.new).map_err(Broke)?;
        let message = format!(
            "{}live and confirmed: {went}{}{}{}",
            if self.live { "already " } else { "" },
            self.riding_said(),
            self.url
                .as_ref()
                .map(|u| format!(" (up as {u})"))
                .unwrap_or_default(),
            self.held_back()
        );
        note(cfg, Outcome::Ok, &message, self.behind);
        if let Some(boot) = &cfg.tools.boot {
            let queued = Command::new(boot)
                .arg("status")
                .output()
                .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("trial queued: /"));
            if queued {
                println!(
                    "the new kernel boots once at the next trial window ({} trial)",
                    name(boot)
                );
            }
        }
        Ok(())
    }

    /// The operator's checkout, pulled up to main when it's on main with
    /// nothing changed.
    fn bring_operator_along(&self) {
        let cfg = self.cfg;
        let git = |args: &[&str]| {
            cfg.run_as_user("git")
                .arg("-C")
                .arg(&cfg.flake)
                .args(args)
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .filter(|o| o.status.success())
        };
        let on_main = git(&["branch", "--show-current"])
            .is_some_and(|o| String::from_utf8_lossy(&o.stdout).trim_end() == "main");
        if on_main && git(&["diff", "--quiet", "HEAD"]).is_some() {
            let _ = cfg
                .run_as_user("git")
                .arg("-C")
                .arg(&cfg.flake)
                .args(["pull", "-q", "--ff-only", "origin", "main"])
                .status();
        }
    }
}

/// A program's name, as a run says it.
pub(crate) fn name(path: &Path) -> String {
    crate::status::name(path)
}

fn copy(from: &Path, to: &Path) {
    if let Err(e) = fs::copy(from, to) {
        eprintln!("{}: {e}", to.display());
    }
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
