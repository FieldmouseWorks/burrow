//! A judgment's look, the producer's side ([`Looks`]): the judging engine
//! run in a unit of its own (`UNIT@ID.service`, burrow-producer's
//! [`Units`]), as a user systemd makes for it, which only the producer's
//! user may start and stop. Nothing of it is `burrow.user`'s, the user
//! authoring sessions run as, so none of them can answer in the judge's
//! place: not the brief, not the checkout it reads, not the answer.
//!
//! - It reads the PR's discussion whole, once, and refuses unless that is
//!   what the request's `context` digests: what the engine is shown is
//!   what the judgment is sealed on.
//! - It fetches the head and its base into the producer's own mirror, as
//!   the producer's App, and lays out the look's folder under the
//!   producer's state ([`lay`]): the brief, the head and its base checked
//!   out, the diff between them, and an empty answer, which is the one file
//!   in it the look may write. The look sees that folder and nothing else
//!   of the producer's.
//! - The look's one way out is a [`Gate`] the producer opens in that folder
//!   for it, to its engine's API alone.
//! - It answers what the look decided ([`Looked`]), by the session and
//!   model the engine reported, at the effort it was run at.
//!
//! A head no CI reported on is built first, before the look's folder is
//! laid out ([`crate::build`]), and the look is shown what came of it; so
//! is one a judge was owed on with its reasons unrecorded, when the
//! producer finds no CI reported on it either. The answer's reasons
//! record what was built and how it came out. A head the repo's flow
//! names nothing to build it with is refused: nothing is judged unbuilt.
use crate::brief::Ci;
use crate::build::{self, Built};
use crate::discussion::{Discussion, current};
use crate::egress::{Dial, Gate};
use crate::producer::Judges;
use crate::{Config, Read, brief};
use burrow_producer::jobs;
use burrow_producer::serve::Work;
use burrow_producer::units::{Ended, Units, run_id};
use conary_core::plugin::{
    Answer, Context, Executed, Identity, Judgment, Request, Response, Task, Trigger,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The engine a look runs: Claude Code, the one engine proj routes to.
pub const ENGINE: &str = "claude";

/// What's in a look's folder: what it's told, the head and its base, the
/// diff between them, its gate, and its answer.
pub const SPEC: &str = "spec.json";
pub const HEAD: &str = "head";
pub const BASE: &str = "base";
pub const DIFF: &str = "changes.diff";
pub const GATE: &str = "gate.sock";
pub const ANSWER: &str = "answer.json";

/// What a look is: the producer writes it, and the look only reads it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub prompt: String,
    pub model: String,
    pub effort: Option<String>,
}

/// What a look decided, as it leaves it in its answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Looked {
    pub verdict: Judgment,
    pub why: String,
    /// The engine's own session, and the model it reports it ran.
    pub session: String,
    pub model: String,
}

impl Looked {
    /// A look's answer, if it reads and says something, by a session.
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        let looked: Self = serde_json::from_slice(bytes)
            .map_err(|e| format!("the judge's look left no answer that reads: {e}"))?;
        if looked.why.trim().is_empty() {
            return Err("the judge's look gave no reasons for its verdict".into());
        }
        if looked.session.trim().is_empty() {
            return Err("the judge's look names no session".into());
        }
        Ok(looked)
    }
}

/// The producer's looks.
pub struct Looks {
    pub cfg: Config,
    pub read: Read,
    pub units: Box<dyn Units>,
    /// The builds' units, for a head no CI reported on.
    pub builds: Box<dyn Units>,
    /// How the gates reach the engine's API.
    pub dial: Dial,
}

impl Config {
    /// Look `id`'s folder.
    pub fn look(&self, id: &str) -> PathBuf {
        self.state.join("looks").join(id)
    }

    /// `repo`'s mirror, which only the producer reads.
    pub fn mirror(&self, repo: &str) -> PathBuf {
        self.state
            .join("git")
            .join(format!("{}.git", repo.to_lowercase()))
    }
}

impl Looks {
    pub fn new(
        cfg: Config,
        read: Read,
        units: Box<dyn Units>,
        builds: Box<dyn Units>,
        dial: Dial,
    ) -> Result<Self, String> {
        for dir in ["git", "looks", "runs"] {
            let dir = cfg.state.join(dir);
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o750)
                .create(&dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(Self {
            cfg,
            read,
            units,
            builds,
            dial,
        })
    }

    /// At startup: stop every look and build a restart left, and clear
    /// their folders (their requests went with them). How many it stopped.
    pub fn reconcile(&self) -> Result<usize, String> {
        let mut stopped = 0;
        for (units, dir) in [(&self.units, "looks"), (&self.builds, "runs")] {
            let loaded = units.loaded()?;
            for id in &loaded {
                units.stop(id)?;
            }
            stopped += loaded.len();
            let dir = self.cfg.state.join(dir);
            for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
                let path = entry.map_err(|e| e.to_string())?.path();
                fs::remove_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
        Ok(stopped)
    }

    /// `head`'s tree, as the mirror has it once it's fetched.
    fn tree(&self, repo: &str, head: &str) -> Result<String, String> {
        let mirror = self.cfg.mirror(repo).to_string_lossy().into_owned();
        let tree = git(
            &self.cfg.git,
            &[
                "--git-dir",
                &mirror,
                "rev-parse",
                "--verify",
                "-q",
                &format!("{head}^{{tree}}"),
            ],
        )?;
        Ok(tree.trim().to_owned())
    }

    /// `head` and `base` fetched into `repo`'s mirror, as the App, whose
    /// environment the producer runs in names git's only credential helper
    /// (github.nix).
    fn fetch(&self, repo: &str, head: &str, base: &str) -> Result<(), String> {
        let mirror = self.cfg.mirror(repo);
        let at = mirror.to_string_lossy().into_owned();
        if !mirror.is_dir() {
            git(&self.cfg.git, &["init", "-q", "--bare", &at])?;
        }
        let url = format!("{}/{repo}.git", self.cfg.github.trim_end_matches('/'));
        git(
            &self.cfg.git,
            &[
                "-C",
                &at,
                "fetch",
                "-q",
                "--no-tags",
                &url,
                &format!("+{head}:refs/judge/{head}"),
                &format!("+{base}:refs/judge/{base}"),
            ],
        )
        .map(drop)
    }
}

impl Judges for Looks {
    /// A head no CI reported on that the repo's flow names nothing to
    /// build with, or a role a look can't run.
    fn refuses(&self, request: &Request) -> Result<(), String> {
        let Task::Judge {
            repo, head, why, ..
        } = &request.task
        else {
            return Err("the judge judges alone".into());
        };
        if why.contains(&Trigger::NoCi) {
            build::unbuildable(&self.cfg.flow(repo)?, repo, head)?;
        }
        let offer = jobs::offered(&self.cfg.settings, &self.cfg.role)?;
        if offer.engine != ENGINE {
            return Err(format!(
                "the {} role runs on {}, and a judge's look runs {ENGINE} alone",
                self.cfg.role, offer.engine
            ));
        }
        Ok(())
    }

    fn judge(&self, request: &Request, work: &Work) -> Result<Response, String> {
        crate::held(request)?;
        let Task::Judge {
            repo,
            pr,
            head,
            against,
            why,
            context,
            ..
        } = &request.task
        else {
            return Err("the judge judges alone".into());
        };
        self.refuses(request)?;
        let choice = jobs::chosen(request.execution.as_ref().and_then(|e| e.choice.as_ref()))?;
        let model = choice.model.clone().ok_or("the flow chose no model")?;

        let source = (self.read)()?;
        let (pull, _) = current(&*source, repo, *pr, head, against)?;
        let discussion = Discussion::read(&*source, repo, *pr)?;
        if discussion.context() != *context {
            return Err(format!(
                "{repo}#{pr}'s discussion isn't the one the judgment was asked on"
            ));
        }
        let reported = if why.contains(&Trigger::NoCi) {
            Some(Vec::new())
        } else if why.contains(&Trigger::Started) {
            Some(reported(&*source, &self.cfg.flow(repo)?, repo, head)?)
        } else {
            None
        };
        drop(source);

        self.fetch(repo, head, &pull.base.sha)?;
        // Built before the look's folder is laid out, so nothing a build
        // does reaches what the look reads.
        let ci = match reported {
            None => Ci::Green,
            Some(checks) if !checks.is_empty() => Ci::Reported(checks),
            Some(_) => {
                let tree = self.tree(repo, head)?;
                let flow = self.cfg.flow(repo)?;
                Ci::Built(build::build(
                    &self.cfg,
                    &*self.builds,
                    &flow,
                    (repo, head, &tree),
                    work,
                )?)
            }
        };
        let built: &[Built] = match &ci {
            Ci::Built(built) => built,
            _ => &[],
        };

        let id = run_id();
        let dir = self.cfg.look(&id);
        let spec = Spec {
            prompt: brief::task(&brief::Judge {
                repo,
                pr: *pr,
                head,
                base: &pull.base.name,
                title: &pull.title,
                body: pull.body.as_deref().unwrap_or(""),
                why,
                owner: &self.cfg.owner,
                discussion: &discussion,
                dir: &dir,
                ci: &ci,
            }),
            model,
            effort: choice.effort.clone(),
        };
        let ran = lay(&self.cfg, &id, repo, head, &pull.base.sha, &spec).and_then(|()| {
            let gate = Gate::open(&dir.join(GATE), &self.cfg.hosts, self.dial.clone())
                .map_err(|e| format!("its gate: {e}"))?;
            let until = Instant::now() + Duration::from_secs(self.cfg.timeout + 60);
            let ended = self.units.run(&id, work, until);
            drop(gate);
            ended
        });
        // Stopped, and read, once nothing of it is left running; its folder
        // goes either way.
        let stopped = self.units.stop(&id);
        let said = fs::read(dir.join(ANSWER));
        let _ = fs::remove_dir_all(&dir);
        match ran? {
            Ended::Exited(0) => {}
            Ended::Exited(n) => return Err(format!("the judge's look failed ({n})")),
            Ended::Broke(why) => return Err(format!("the judge's look broke: {why}")),
        }
        stopped.map_err(|e| format!("the judge's look wouldn't stop: {e}"))?;
        let looked = Looked::read(&said.map_err(|e| format!("its answer: {e}"))?)?;
        if looked.verdict == Judgment::Pass && !built.is_empty() && !build::passed(built) {
            return Err(
                "the judge's look passed a head none of whose checks passed when it was built: nothing is judged unbuilt"
                    .into(),
            );
        }
        // What was built, recorded by the producer, whatever the look said.
        let why = if built.is_empty() {
            looked.why
        } else {
            format!("{}\n\n{}", looked.why.trim(), build::summary(built))
        };
        Ok(Response {
            contract: request.contract.clone(),
            by: Identity {
                engine: ENGINE.into(),
                session: Some(looked.session),
            },
            execution: Some(Executed {
                model: Some(looked.model),
                effort: spec.effort,
                context: Context::Fresh,
            }),
            answer: Answer::Judge {
                verdict: looked.verdict,
                why,
            },
        })
    }
}

/// What GitHub reports of CI on `repo`'s head `head`, as Conary's flow
/// counts it (`flow::ci`): each check run but the judge's own, and each
/// status but the steward's `review-settled`, the train's and one of the
/// flow's proofs', with who posted it (anyone who can push can post one).
fn reported(
    source: &dyn conary_github::flow::Read,
    flow: &conary_github::flow::Config,
    repo: &str,
    head: &str,
) -> Result<Vec<String>, String> {
    let runs = source
        .check_runs(repo, head)
        .map_err(|e| format!("{head}'s check runs: {e}"))?;
    let statuses = source
        .statuses(repo, head)
        .map_err(|e| format!("{head}'s statuses: {e}"))?;
    let runs = runs
        .into_iter()
        .filter(|r| r.name != crate::producer::CHECK)
        .map(|r| format!("{}: {}", r.name, r.conclusion.unwrap_or(r.status)));
    let not_ci = |context: &str| {
        let context = context.to_lowercase();
        context == "review-settled"
            || context == conary_github::train::CONTEXT
            || flow
                .settings
                .proofs
                .keys()
                .any(|p| p.eq_ignore_ascii_case(&context))
    };
    let statuses = statuses
        .into_iter()
        .filter(|s| !not_ci(&s.context))
        .map(|s| {
            let by = s.creator.map_or("no one GitHub names".into(), |u| u.login);
            format!("{}: {} (a status, posted by {by})", s.context, s.state)
        });
    Ok(runs.chain(statuses).collect())
}

/// git with `args`: what it printed, or why it failed.
fn git(program: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git {}: {}",
            args.iter()
                .find(|a| !a.starts_with('-') && !a.contains('/'))
                .copied()
                .unwrap_or_default(),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Look `id`'s folder, laid out for it from `repo`'s mirror: its spec, the
/// head and the base checked out, the diff from their merge base to the
/// head, and an empty answer, the one file the look may write. The rest is
/// the producer's, which the look's user, in its group, only reads.
pub fn lay(
    cfg: &Config,
    id: &str,
    repo: &str,
    head: &str,
    base: &str,
    spec: &Spec,
) -> Result<(), String> {
    let dir = cfg.look(id);
    let at = |e: std::io::Error, path: &Path| format!("{}: {e}", path.display());
    fs::DirBuilder::new()
        .mode(0o750)
        .create(&dir)
        .map_err(|e| at(e, &dir))?;
    let path = dir.join(SPEC);
    fs::write(&path, serde_json::to_vec(spec).map_err(|e| e.to_string())?)
        .and_then(|()| fs::set_permissions(&path, fs::Permissions::from_mode(0o640)))
        .map_err(|e| at(e, &path))?;
    let mirror = cfg.mirror(repo).to_string_lossy().into_owned();
    for (name, commit) in [(HEAD, head), (BASE, base)] {
        let tree = dir.join(name);
        fs::DirBuilder::new()
            .mode(0o750)
            .create(&tree)
            .map_err(|e| at(e, &tree))?;
        // An index of its own, so looks at once don't share the mirror's.
        let index = dir.join(format!(".{name}.index"));
        let out = Command::new(&cfg.git)
            .arg("--git-dir")
            .arg(&mirror)
            .arg("--work-tree")
            .arg(&tree)
            // A link is checked out as a file holding where it points: the
            // look reads the head's text, never what a link in it names.
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.symlinks=false",
            ])
            .args(["checkout", "-q", "-f"])
            .arg(commit)
            .args(["--", "."])
            .env("GIT_INDEX_FILE", &index)
            // LFS files stay their pointers: the look judges text, and the
            // mirror, fetched by URL, names no endpoint for LFS to fetch
            // their objects from.
            .env("GIT_LFS_SKIP_SMUDGE", "1")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("git: {e}"))?;
        let _ = fs::remove_file(&index);
        if !out.status.success() {
            return Err(format!(
                "{commit} checked out: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    let diff = git(
        &cfg.git,
        &[
            "--git-dir",
            &mirror,
            "diff",
            "--no-color",
            "--no-ext-diff",
            &format!("{base}...{head}"),
        ],
    )?;
    let path = dir.join(DIFF);
    fs::write(&path, diff)
        .and_then(|()| fs::set_permissions(&path, fs::Permissions::from_mode(0o640)))
        .map_err(|e| at(e, &path))?;
    let path = dir.join(ANSWER);
    fs::write(&path, b"")
        .and_then(|()| fs::set_permissions(&path, fs::Permissions::from_mode(0o660)))
        .map_err(|e| at(e, &path))?;
    Ok(())
}
