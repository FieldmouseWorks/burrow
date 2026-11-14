//! renovate-sweep: Renovate PRs that can't merge themselves get a Claude.
//!
//! Renovate merges its own PRs once CI is green. The ones it can't (CI red,
//! or a major version, which needs a verdict first) would otherwise sit
//! there. This hands a repo's stuck PRs to one Claude job in that project,
//! which fixes them on their branches so they can merge. Once per PR head,
//! so a fix that's pushed gets a fresh look only if it's still stuck. The
//! autoland steward's own status (`settled`) doesn't count as a check. With
//! the machine's own App, it and its jobs act on GitHub as that App.
//!
//! The job's verdict on a major: ok when it can merge as it is, held when
//! it shouldn't (its comment says why). The job ends that comment with a
//! marker naming the head it cleared and the base it was cleared for,
//! sealed, `<!-- major-verdict: ok|held SHA on BASE MAC -->`, and labels the
//! PR major-ok or major-held for people to see. The sweep seals both verdicts
//! on the head and base it hands a major over on (major-seal) and puts the
//! markers in the task, as the steward gives a judge its seals: the job runs
//! nothing of the sweep's, whose state its sandbox needn't see (a Codex
//! job's doesn't), and can seal no head it wasn't handed. A verdict counts only for the
//! head and base it names, only from our own login and only with its seal,
//! so a job still finishing on an older head, a push since, a PR retargeted
//! since, someone else's comment or another job quoting a marker can't
//! clear a PR. The autoland steward merges an ok one once its CI is green
//! (the same rule) in the repos it covers (steward.rs). The owner gets one
//! notice listing the held ones, as waiting on a decision, and the ok ones
//! no steward will merge.
//!
//! Security fixes (Renovate's `security` label) merge themselves even when
//! they're majors: none is asked for a verdict or waits for one. A red one
//! is handed over again every day, not once per head: the patch window is
//! short, and a cause outside the repo (a broken runner) can clear without
//! a push. One still open a day after Renovate opened it goes in a notice,
//! whatever is holding it.
//!
//! It runs as the user; what it runs, it runs by path (`Config::tools`).

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use rustix::fs::{FlockOperation, flock};
use serde::Deserialize;
use serde_json::Value;

use crate::{complain, say, trimmed};

mod pr;
mod request;
mod scan;
mod steward;
mod task;
mod wake;

use pr::{Pr, State};
use request::{REQUEST, TITLE};
use steward::Steward;

/// The labels a job puts on a major it read.
pub(crate) const OK: &str = "major-ok";
pub(crate) const HELD: &str = "major-held";
/// How long a refresh request is given before it's asked again: Renovate
/// runs within the hour.
const RUN_COMES: i64 = 3600;
/// How long a security fix may stay open before it's a notice.
const SECURITY_DAY: i64 = 86400;

/// What modules/updates.nix tells renovate-sweep.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Config {
    /// renovate-sweep, as the machine names it: how it says itself.
    pub name: String,
    /// Its own: what each PR was last handed over for (OWNER_REPO-N), its
    /// refresh requests taken back (OWNER_REPO-N.released), and the key
    /// major-seal seals verdicts with.
    pub state: PathBuf,
    /// Where the machine's projects are: each clone of a GitHub repo, a
    /// project its job runs in.
    pub projects: PathBuf,
    /// Who decides a major that's held, as the task names them.
    pub owner: String,
    /// The autoland steward's own commit status, which isn't CI.
    pub settled: String,
    /// The repos the steward covers, as it last found them (its
    /// repos.json): none without it.
    pub covered: Option<PathBuf>,
    /// The machine's own App, when the jobs act as it.
    pub app: Option<App>,
    /// Who Renovate comments as, as REST spells it: its own App's bot
    /// (burrow.updates.renovate.app), which needn't be the jobs'. None:
    /// Renovate runs as the user, whose gh login the sweep's is too.
    pub renovate: Option<String>,
    /// The longest a Renovate run goes on, in seconds: a refresh request
    /// taken back waits that long before a job may push.
    pub renovate_run: u64,
    pub tools: Tools,
}

/// The App the jobs act on GitHub as.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct App {
    /// As proj run --app names it.
    pub name: String,
    /// Its bot's login as GraphQL spells it: who the jobs post as.
    pub bot: String,
}

/// What it runs, by path.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Tools {
    /// gh, as the App when there's one.
    pub gh: PathBuf,
    pub git: PathBuf,
    pub proj: PathBuf,
    /// HOST-notify, which tells the owner.
    pub notify: PathBuf,
    /// major-seal, which seals the verdicts a job is handed, and checks one.
    pub seal: PathBuf,
    /// lock-wait (modules/autoland/lock_wait.py): when a lock file refresh that
    /// lock-age refused for age only can go again, and how far the sweep
    /// goes asking.
    pub lock_wait: PathBuf,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    fn github(&self) -> burrow_github::Gh {
        burrow_github::Gh {
            program: Some(self.tools.gh.clone().into_os_string()),
            ..Default::default()
        }
    }

    fn pages(&self, path: &str) -> Option<Vec<Value>> {
        let pages = burrow_github::pages(&mut self.github(), path).ok()?;
        pages.iter().all(Value::is_array).then_some(pages)
    }

    /// The sweep over `only`, or over every repo and the notices: what it
    /// exits with.
    pub fn run(&self, only: Option<&str>) -> u8 {
        match self.sweep(only) {
            Ok(true) => 0,
            Ok(false) => 1,
            Err(Broke(why)) => {
                complain(&format!("{}: {why}", self.name));
                1
            }
        }
    }

    /// Whether everything it did worked.
    fn sweep(&self, only: Option<&str>) -> Result<bool, Broke> {
        // One sweep at a time, the full one and each repo's alike: each
        // reads the keys only once the one before has kept what it handed
        // over, so no two hand the same head to a job. A job doesn't hold
        // it (Rust opens it close-on-exec).
        let path = self.state.join("sweep.lock");
        let broke = |e: io::Error| Broke(format!("{}: {e}", path.display()));
        fs::create_dir_all(&self.state).map_err(broke)?;
        let lock = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(broke)?;
        flock(&lock, FlockOperation::LockExclusive).map_err(|e| broke(e.into()))?;
        // Who the jobs post as, whose verdicts are read: the App's bot, or
        // the user's own login. A user can hold the bot's name in GraphQL's
        // spelling, but not the seal. Who Renovate comments as, as REST
        // spells it: its App's bot, SLUG[bot] there, a name no person's can
        // be, or the user's own login when it runs as them.
        let user = || {
            burrow_github::call(&mut self.github(), "GET", "user", None, None)
                .ok()
                .and_then(|r| r.json().ok())
                .and_then(|v| v["login"].as_str().map(str::to_owned))
                .unwrap_or_default()
        };
        let me = match &self.app {
            Some(app) => app.bot.clone(),
            None => user(),
        };
        let renovate = match &self.renovate {
            Some(bot) => bot.clone(),
            None if self.app.is_none() => me.clone(),
            None => user(),
        };
        let limits = self.limits()?;
        let who = Who {
            me,
            renovate,
            limits,
        };
        let steward = Steward::read(self.covered.as_deref());
        let mut pass = Pass::default();
        for project in projects(&self.projects) {
            let Some(repo) = self.origin(&self.projects.join(&project)) else {
                continue;
            };
            // A hidden folder counts only when it's named for its repo
            // (o/.github in .github).
            let own = repo.split_once('/').map_or("", |(_, own)| own);
            if project.starts_with('.') && project.to_lowercase() != own.to_lowercase() {
                continue;
            }
            if only.is_some_and(|only| only.to_lowercase() != repo.to_lowercase()) {
                continue;
            }
            self.repo(&project, &repo, &who, &steward, &mut pass);
        }
        // The notices list every repo's: one repo's pass leaves them be.
        if only.is_none() {
            self.notices(&mut pass);
        }
        Ok(!pass.failed)
    }

    /// One repo's stuck PRs, all to one job: they usually share a cause (a
    /// toolchain bump, a lint the new versions trip), and one job per PR
    /// would fix it several times over, side by side.
    fn repo(&self, project: &str, repo: &str, who: &Who, steward: &Steward, pass: &mut Pass) {
        let info = steward.repo(repo, now());
        let prs = match self.list(repo) {
            Ok(prs) => prs,
            Err(Broke(why)) => {
                complain(&why);
                pass.failed = true;
                return;
            }
        };
        let mut sweep = Sweep {
            cfg: self,
            repo,
            who,
            info: &info,
            labelled: false,
            next: None,
        };
        let mut handed = Vec::new();
        for pr in &prs {
            match sweep.pr(Pr(pr), pass) {
                Ok(Some(h)) => handed.push(h),
                Ok(None) => {}
                Err(Broke(why)) => {
                    complain(&format!("{}: {why}", self.name));
                    pass.failed = true;
                    return;
                }
            }
        }
        if let Err(Broke(why)) = self.book(repo, sweep.next) {
            complain(&why);
            pass.failed = true;
            return;
        }
        // Each one's key is kept before its job starts, so a key that can't
        // be kept hands nothing over pass after pass; a job that doesn't
        // start gives them back to the next pass.
        let mut kept = Vec::new();
        let mut was = Vec::new();
        for h in handed {
            let before = fs::read_to_string(&h.key).ok();
            match write(&h.key, &format!("{}\n", h.want)) {
                Ok(()) => {
                    kept.push(h);
                    was.push(before);
                }
                Err(Broke(why)) => {
                    complain(&format!("{}: {why}", self.name));
                    pass.failed = true;
                }
            }
        }
        if kept.is_empty() {
            return;
        }
        if let Err(Broke(why)) = self.hand(project, repo, &info, &kept) {
            complain(&format!("{}: {why}", self.name));
            pass.failed = true;
            for (h, before) in kept.iter().zip(was) {
                let back = match before {
                    Some(text) => write(&h.key, &text),
                    None => fs::remove_file(&h.key)
                        .map_err(|e| Broke(format!("{}: {e}", h.key.display()))),
                };
                if let Err(Broke(why)) = back {
                    complain(&format!("{}: {why}", self.name));
                }
            }
        }
    }

    /// `handed` to one Claude job in `project`.
    fn hand(
        &self,
        project: &str,
        repo: &str,
        info: &steward::Repo,
        handed: &[Handed],
    ) -> Result<(), Broke> {
        let mut seals = String::new();
        for h in handed {
            if let Some(m) = &h.major {
                let [ok, held] = ["ok", "held"].map(|v| self.seal(repo, m, v));
                seals.push_str(&format!(
                    "- #{} on {} for {}: ok `{}`, held `{}`\n",
                    m.n, m.sha, m.base, ok?, held?
                ));
            }
        }
        let todo: String = handed.iter().map(|h| h.line.as_str()).collect();
        let task = task::task(
            self,
            repo,
            todo.trim_end_matches('\n'),
            &info.proofs,
            seals.trim_end_matches('\n'),
        );
        let mut proj = Command::new(&self.tools.proj);
        // The kind of work it is, which the machine may give a model of its own.
        proj.args(["run", "--pipeline", "--role", "renovate-sweep"]);
        if let Some(app) = &self.app {
            proj.args(["--app", &app.name]);
        }
        let ran = proj
            .args([project, &task])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status();
        let why = match ran {
            Ok(s) if s.success() => {
                say(&format!(
                    "{repo}: {} PRs handed to one Claude job",
                    todo.matches('\n').count()
                ));
                return Ok(());
            }
            Ok(s) => format!("{repo}: proj run: {s}"),
            Err(e) => format!("{}: {e}", self.tools.proj.display()),
        };
        Err(Broke(why))
    }

    /// The marker major-seal seals verdict `v` on major `m` with, in `repo`.
    fn seal(&self, repo: &str, m: &Major, v: &str) -> Result<String, Broke> {
        let seal = &self.tools.seal;
        let out = Command::new(seal)
            .args([repo, &m.n, v, &m.sha, &m.base])
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| Broke(format!("{}: {e}", seal.display())))?;
        let marker = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !out.status.success() || !marker.starts_with("<!-- ") {
            return Err(Broke(format!(
                "{repo}: {} {} {v}: {}",
                seal.display(),
                m.n,
                out.status
            )));
        }
        Ok(marker)
    }

    /// The majors waiting on the owner, in one notice: the held ones apart
    /// from the ok ones in repos no steward merges in. And the security
    /// fixes stuck a day.
    fn notices(&self, pass: &mut Pass) {
        let held = pass.held.trim_end_matches('\n');
        let ready = pass.ready.trim_end_matches('\n');
        let mut parts = Vec::new();
        if !held.is_empty() {
            parts.push(format!(
                "A Claude job read these and held them; its comment on each PR says why. They need a decision before they merge:\n{held}"
            ));
        }
        if !ready.is_empty() {
            parts.push(format!(
                "A Claude job read these and cleared them, and their CI is green, but the autoland steward doesn't cover their repos, so nothing merges them:\n{ready}"
            ));
        }
        let majors = if parts.is_empty() {
            self.notify(&["close", "majors", "No major updates waiting."])
        } else {
            self.notify(&[
                "open",
                "majors",
                "Major updates waiting on you",
                &parts.join("\n\n"),
            ])
        };
        let stuck = pass.stuck.trim_end_matches('\n');
        let security = if stuck.is_empty() {
            self.notify(&["close", "security", "No security updates stuck."])
        } else {
            self.notify(&[
                "open",
                "security",
                "Security updates stuck",
                &format!(
                    "Renovate opened these security fixes over a day ago and they haven't merged. A red one goes to a Claude job every day; if the same one keeps failing, the cause is probably outside the repo (its CI runner, a broken default branch):\n{stuck}"
                ),
            ])
        };
        pass.failed |= !(majors && security);
    }

    /// HOST-notify `words`: whether it worked.
    fn notify(&self, words: &[&str]) -> bool {
        let notify = &self.tools.notify;
        let why = match Command::new(notify)
            .args(words)
            .stdin(Stdio::null())
            .status()
        {
            Ok(s) if s.success() => return true,
            Ok(s) => format!("{} {}: {s}", notify.display(), words[0]),
            Err(e) => format!("{}: {e}", notify.display()),
        };
        complain(&format!("{}: {why}", self.name));
        false
    }

    /// How far it goes asking for a lock file refresh and taking a request
    /// back (lock-wait --limits, which the steward's deadline counts them
    /// from too).
    fn limits(&self) -> Result<Limits, Broke> {
        let lock_wait = &self.tools.lock_wait;
        let out = Command::new(lock_wait)
            .arg("--limits")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| Broke(format!("{}: {e}", lock_wait.display())))?;
        let said = String::from_utf8_lossy(&out.stdout);
        let n: Vec<i64> = said
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .map_while(|w| w.parse().ok())
            .collect();
        match n[..] {
            [asks, standing, releases, release_gap, pass] if out.status.success() && pass > 0 => {
                Ok(Limits {
                    asks,
                    standing,
                    releases,
                    release_gap,
                    pass,
                })
            }
            _ => Err(Broke(format!(
                "{} --limits: {}, saying {:?}",
                lock_wait.display(),
                out.status,
                said.trim_end()
            ))),
        }
    }

    /// The GitHub repo the clone at `dir` is of, as its remote spells it.
    fn origin(&self, dir: &Path) -> Option<String> {
        let mut git = Command::new(&self.tools.git);
        git.arg("-C").arg(dir).args(["remote", "get-url", "origin"]);
        let out = git
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let url = trimmed(out.stdout);
        let repo = url
            .strip_prefix("https://github.com/")
            .or_else(|| url.strip_prefix("git@github.com:"))
            .unwrap_or(&url);
        let repo = repo.strip_suffix(".git").unwrap_or(repo);
        repo.contains('/').then(|| repo.to_string())
    }

    /// What `program args` printed, whatever it exited with; what it says
    /// on stderr goes nowhere.
    fn said(&self, program: &Path, args: &[&str]) -> Vec<u8> {
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map(|o| o.stdout)
            .unwrap_or_default()
    }

    /// `program args`, all it says going nowhere, and whatever comes of it.
    fn quietly(&self, program: &Path, args: &[&str]) {
        let _ = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Who the sweep reads and asks as, and how far it asks.
struct Who {
    /// Whose sealed verdicts count: the jobs' login.
    me: String,
    /// Who Renovate comments as, as REST spells it.
    renovate: String,
    limits: Limits,
}

/// How far it goes asking Renovate for a lock file refresh, and taking a
/// request back (lock-wait --limits).
struct Limits {
    /// Requests on a head.
    asks: i64,
    /// How long each may stand, in seconds.
    standing: i64,
    /// Tries at taking one back.
    releases: i64,
    /// How far apart, in seconds.
    release_gap: i64,
    /// Maximum wait before revisiting a refresh.
    pass: i64,
}

/// What a whole pass tells the owner, and whether all of it worked.
#[derive(Default)]
struct Pass {
    held: String,
    ready: String,
    stuck: String,
    failed: bool,
}

/// A PR handed to the repo's job: its line in the task, the head and base
/// it's handed over on if it's a major (the verdicts the job is handed are
/// sealed on those), and what it's kept as handed over for once the job has
/// it.
struct Handed {
    line: String,
    major: Option<Major>,
    key: PathBuf,
    want: String,
}

/// A major as it's handed over: its number, head and base.
struct Major {
    n: String,
    sha: String,
    base: String,
}

/// Something it needed broke.
struct Broke(String);

/// One repo's sweep.
struct Sweep<'a> {
    cfg: &'a Config,
    repo: &'a str,
    who: &'a Who,
    info: &'a steward::Repo,
    /// Whether the labels a job puts on a major are there yet.
    labelled: bool,
    /// Earliest refresh deadline in this repo.
    next: Option<i64>,
}

impl Sweep<'_> {
    fn wake(&mut self, at: i64) {
        self.next = Some(self.next.map_or(at, |was| was.min(at)));
    }

    /// One of its Renovate PRs: what it's handed over for, if it is.
    fn pr(&mut self, pr: Pr, pass: &mut Pass) -> Result<Option<Handed>, Broke> {
        let (cfg, repo, who) = (self.cfg, self.repo, self.who);
        let n = pr.text("number");
        let sha = pr.text("headRefOid");
        let base = pr.text("baseRefName");
        let title = pr.text("title");
        let branch = pr.text("headRefName");
        // A proof the steward has run on a machine (proofs) is no CI to
        // wait for: a major's job runs it itself.
        let checks = pr.checks(&cfg.settled, &self.info.proof_names);
        let count = |f: &dyn Fn(&pr::Check) -> bool| checks.iter().filter(|c| f(c)).count();
        let red = count(&|c| c.state == State::Red);
        let pending = count(&|c| c.state == State::Pending);
        let running = count(&|c| c.state == State::Pending && !c.proof);
        let finished = pr::finished(&checks);
        // No checks at all isn't "passed CI": nothing has tested it yet.
        let untested = checks.is_empty();
        let major = pr.major();
        let security = pr.security();
        let key = cfg.state.join(format!("{}-{n}", repo.replace('/', "_")));
        let seen = fs::read(&key).map(trimmed).unwrap_or_default();
        // A major's key also says the job was handed sealed verdicts on
        // this base, so one retargeted since (or handed over before its
        // task carried them) is handed over once more.
        let mut want = sha.clone();
        if major {
            want = format!("{sha} verdicts sealed for {base}");
        }
        if security {
            want = format!("{sha} {}", jiff::Zoned::now().strftime("%Y-%m-%d"));
        }
        // Red since it was handed over (a proof run later, say): another
        // job fixes that, verdict or not. Otherwise a major's key names the
        // CI its job was handed: CI that finishes again on the same head,
        // after a job reran it and left (as LoK-web#598's did) or turned up
        // after the job saw none, brings another job if there's still no
        // verdict.
        if red > 0 {
            want.push_str(" red");
        } else if major {
            want.push_str(&format!(" after CI {finished}"));
        }
        // The newest one with its seal, of our own login's for this head on
        // this base.
        let mut verdict = String::new();
        if major && !who.me.is_empty() {
            let said = cfg.said(
                &cfg.tools.gh,
                &["pr", "view", &n, "-R", repo, "--json", "comments"],
            );
            for (v, mac) in pr::verdicts(&said, &who.me, &sha, &base) {
                if self.sealed(&n, &v, &sha, &base, &mac) {
                    verdict = v;
                }
            }
        }
        let url = format!("https://github.com/{repo}/pull/{n}");
        if major && verdict == "held" {
            pass.held.push_str(&format!("- {title}: {url}\n"));
        } else if major && verdict == "ok" && !self.info.covered && red == 0 && pending == 0 {
            // One with no CI at all is listed too, saying so: nothing else
            // would bring it to anyone.
            let note = if untested {
                " (no checks ran on it)"
            } else {
                ""
            };
            pass.ready.push_str(&format!("- {title}: {url}{note}\n"));
        }
        if security && pr.created().is_some_and(|at| now() - at > SECURITY_DAY) {
            let why = if red > 0 {
                "its CI fails"
            } else if untested {
                "nothing has tested it"
            } else if pending > 0 {
                "its CI hasn't finished"
            } else {
                "its CI is green and it still hasn't merged"
            };
            pass.stuck.push_str(&format!("- {title}: {url} ({why})\n"));
        }
        // A rebase! title has Renovate redo the branch on each of its runs,
        // each of whose pushes starts the next, until someone takes it off:
        // the sweep does, and leaves the PR for a pass after, which waits out
        // a run that read it (handing over, below).
        if let Some(rest) = title.strip_prefix(TITLE) {
            self.wake(now() + who.limits.pass);
            self.retitle(&n, rest.trim_start());
            return Ok(None);
        }
        // A head whose refresh request was taken back for a job (below) has
        // done with requests: when, and how many times it was asked.
        let released = with_suffix(&key, ".released");
        let (rel_sha, mut rel_tries, mut rel_at) = taken_back(&released);
        if rel_sha != sha {
            (rel_tries, rel_at) = (0, 0);
        }
        // A lock file refresh whose one red check is Renovate's artifacts,
        // from lock-age refusing it only because nothing in it is old
        // enough yet, is no fix for a job: it waits for the time Renovate's
        // comment on it gives (lock-wait). Then Renovate is asked to
        // refresh it again, as it won't by itself while the branch doesn't
        // conflict: the rebase label, which its next run reads, whatever its
        // config, and takes off. Each ask counts, whether GitHub took it,
        // refused it or never answered: the labels on the next pass say
        // which. A request
        // still standing six hours on, or three that went without the head
        // or the time moving, and it's red CI for a job, once a Renovate
        // run that read the last one is over: Renovate having taken it
        // counts as taking it back.
        let reds: Vec<&Value> = checks
            .iter()
            .filter(|c| c.state == State::Red)
            .map(|c| &c.name)
            .collect();
        let standing = pr.labelled(REQUEST);
        // Keep retrying requests and their release without another delivery.
        if standing || reds == [&Value::from("renovate/artifacts")] {
            self.wake(now() + who.limits.pass);
        }
        if reds == [&Value::from("renovate/artifacts")]
            && !who.renovate.is_empty()
            && seen != format!("{sha} red")
            && rel_sha != sha
        {
            let comments = cfg
                .pages(&format!("repos/{repo}/issues/{n}/comments"))
                .and_then(|v| serde_json::to_vec(&v).ok())
                .unwrap_or_default();
            let ripe = self.ripe(&comments);
            if !ripe.is_empty() {
                let now = now();
                if let Ok(at) = ripe.parse::<i64>()
                    && at > now
                {
                    self.wake(at);
                }
                let was: Vec<&str> = seen
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect();
                let word = |i: usize| was.get(i).copied().unwrap_or_default();
                let (mut tries, mut at) = (number(word(3)), number(word(4)));
                if format!("{} {} {}", word(0), word(1), word(2))
                    != format!("{sha} refreshed {ripe}")
                {
                    (tries, at) = (0, 0);
                }
                let limits = &who.limits;
                if ripe.parse::<i64>().is_ok_and(|ripe| now < ripe)
                    || now - at < RUN_COMES
                    || (standing && now - at < limits.standing)
                {
                    return Ok(None);
                } else if !standing && tries < limits.asks {
                    write(
                        &key,
                        &format!("{sha} refreshed {ripe} {} {now}\n", tries + 1),
                    )?;
                    self.request(&n, true);
                    return Ok(None);
                } else if !standing {
                    write(&released, &format!("{sha} 0 {now}\n"))?;
                    return Ok(None);
                }
            }
        }
        if red == 0 && !major {
            return Ok(None);
        }
        // A major waits for its CI to finish, unless some of it is red
        // already: its verdict is read off finished CI, and a job handed
        // one still running has to wait it out. Its CI finishing starts
        // this again. Its proofs it doesn't wait for: its job runs those.
        if red == 0 && running > 0 {
            return Ok(None);
        }
        if seen == want {
            return Ok(None);
        }
        // A job that pushed its fix and then cleared the new head is done:
        // nothing for another job to do there.
        if !verdict.is_empty() && red == 0 {
            write(&key, &format!("{want}\n"))?;
            return Ok(None);
        }
        // A refresh request still standing (the rebase label, ours or anyone's)
        // has Renovate redo the branch over whatever a job pushes, as it
        // does for no other branch someone else pushed to: it comes off
        // first. A Renovate run that read it before may still be going, so
        // the job waits out the longest one. Three times, ten minutes
        // apart, that GitHub kept the label, whether it refused or never
        // answered, and the job is handed it to take off itself.
        let mut request = String::new();
        let now = now();
        let limits = &who.limits;
        if standing && (rel_tries < limits.releases || now - rel_at < limits.release_gap) {
            if now - rel_at >= limits.release_gap {
                write(&released, &format!("{sha} {} {now}\n", rel_tries + 1))?;
                self.request(&n, false);
            }
            return Ok(None);
        } else if standing {
            request = format!(
                "; it has the {REQUEST} label, which has Renovate redo the branch over what you push: take the label off first, and push nothing to it for {} minutes after, while a Renovate run that read it may still be going",
                cfg.renovate_run / 60
            );
        } else if rel_sha == sha && now - rel_at < cfg.renovate_run as i64 {
            self.wake(now + limits.pass);
            return Ok(None);
        }
        // Nor while a run that read a request that came off since (Renovate
        // took it, or someone else did, on this head or an old one) may be
        // going still, or while the PR's timeline can't say.
        if !standing
            && self
                .request_gone(&n)
                .is_none_or(|at| at.is_some_and(|at| now - at < cfg.renovate_run as i64))
        {
            self.wake(now + limits.pass);
            return Ok(None);
        }
        if major {
            // The job labels what it read; the labels have to exist first.
            if !self.labelled {
                for (label, color, what) in [
                    (
                        OK,
                        "0e8a16",
                        "Major update a Claude job read: fine to merge as it is",
                    ),
                    (
                        HELD,
                        "d93f0b",
                        "Major update a Claude job read: needs a decision first (see its comment)",
                    ),
                ] {
                    cfg.quietly(
                        &cfg.tools.gh,
                        &[
                            "label",
                            "create",
                            label,
                            "-R",
                            repo,
                            "--force",
                            "--color",
                            color,
                            "--description",
                            what,
                        ],
                    );
                }
                self.labelled = true;
            }
            // The old head's labels would mislead a reader; the marker is
            // what counts.
            if pr.labelled(OK) || pr.labelled(HELD) {
                cfg.quietly(
                    &cfg.tools.gh,
                    &[
                        "pr",
                        "edit",
                        &n,
                        "-R",
                        repo,
                        "--remove-label",
                        OK,
                        "--remove-label",
                        HELD,
                    ],
                );
            }
        }
        let why = match (major, red > 0) {
            (true, true) => "a major version bump, and its CI fails",
            (true, false) => "a major version bump",
            (false, _) => "its CI fails",
        };
        Ok(Some(Handed {
            line: format!("- #{n} \"{title}\" (branch {branch}): {why}{request}\n"),
            major: major.then_some(Major { n, sha, base }),
            key,
            want,
        }))
    }

    /// Whether major-seal says `mac` seals verdict `v` on PR `n`'s `sha`
    /// for `base`.
    fn sealed(&self, n: &str, v: &str, sha: &str, base: &str, mac: &str) -> bool {
        let seal = &self.cfg.tools.seal;
        match Command::new(seal)
            .args(["--check", self.repo, n, v, sha, base, mac])
            .stdin(Stdio::null())
            .status()
        {
            Ok(s) => s.success(),
            Err(e) => {
                complain(&format!("{}: {}: {e}", self.cfg.name, seal.display()));
                false
            }
        }
    }

    /// When a lock file refresh lock-age refused for age only can go
    /// again, as lock-wait reads Renovate's `comments` on it: nothing when
    /// it isn't such a refresh.
    fn ripe(&self, comments: &[u8]) -> String {
        let lock_wait = &self.cfg.tools.lock_wait;
        let child = Command::new(lock_wait)
            .arg(&self.who.renovate)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn();
        let mut child = match child {
            Ok(child) => child,
            Err(e) => {
                complain(&format!("{}: {}: {e}", self.cfg.name, lock_wait.display()));
                return String::new();
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            use std::io::Write;
            // One that stops reading has its answer all the same.
            let _ = stdin.write_all(comments);
        }
        child
            .wait_with_output()
            .map(|o| trimmed(o.stdout))
            .unwrap_or_default()
    }
}

/// The projects in `dir`, as a shell's `DIR/*/ DIR/.*/` has them: its
/// folders, and links to folders, the hidden ones last.
fn projects(dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    names.sort();
    let (hidden, shown): (Vec<String>, Vec<String>) =
        names.into_iter().partition(|n| n.starts_with('.'));
    shown.into_iter().chain(hidden).collect()
}

/// When a request on PR `key`'s head was last taken back, and how many
/// times: (the head, tries, when), none of it when there's none.
fn taken_back(path: &Path) -> (String, i64, i64) {
    if !path.is_file() {
        return (String::new(), 0, 0);
    }
    let text = fs::read(path).map(trimmed).unwrap_or_default();
    let words: Vec<&str> = text
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect();
    let word = |i: usize| words.get(i).copied().unwrap_or_default();
    (word(0).to_string(), number(word(1)), number(word(2)))
}

/// A number the sweep wrote, 0 when it isn't one.
fn number(word: &str) -> i64 {
    word.parse().unwrap_or(0)
}

/// `path` with `suffix` on its name.
fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(suffix);
    PathBuf::from(name)
}

fn write(path: &Path, text: &str) -> Result<(), Broke> {
    fs::write(path, text).map_err(|e| Broke(format!("{}: {e}", path.display())))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// A JSON value as jq's string interpolation (and `jq -r`) shows it: a
/// string as itself, anything else (none at all as null) as JSON.
pub(crate) fn shown(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => "null".into(),
    }
}

#[cfg(test)]
mod tests;
