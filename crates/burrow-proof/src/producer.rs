//! The proof producer (Conary's `docs/m6-plugins.md`, P): a long-lived
//! service with its own user and its own key, which the flow's proof plugin
//! forwards each `proof` and `prepare` request to, from the flow's
//! controller alone (burrow-producer's socket).
//!
//! - Each check's recipe is the flow's settings' (`settings.proofs`), read
//!   for each request, never the request's or the PR's. A `Job` recipe is a
//!   model's work, never a proof: the request is refused.
//! - The commit's tree is checked against GitHub's, and the commit fetched
//!   into the producer's mirror, which the worker reads.
//! - The work runs as another user, the worker's, in a unit of its own
//!   (`UNIT@ID.service`, [`crate::work`]), which can't read the key: a
//!   proof runs the repo's own code. The verdict is the worker's exit, as
//!   systemd keeps it, and the log is what it printed.
//! - A proof is its own record and check run, started before the work:
//!   once the work has a verdict it's kept, signed for the commit it ran
//!   on alone, and published. Work abandoned or ended with no verdict
//!   withdraws it (its run `cancelled`), and signs nothing.
//! - A `prepare` readies a `Build` recipe's inputs (in the worker's unit
//!   too, in a heavy slot) and posts nothing.
//!
//! Every request runs new work: nothing here is cached, so another commit
//! with the same tree runs again, and is signed for itself.
use crate::plugin::{Bench, Config, ENGINE};
use crate::work::{FAILED, Mode, PASSED, SPEC, Spec};
use burrow_github::{Api, call};
use burrow_producer::publish;
use burrow_producer::records::{Records, Target};
use burrow_producer::serve::Work;
use burrow_producer::sign::{Proof, Signer};
use burrow_producer::units::{Ended, Units, run_id};
use conary_core::plugin::{
    Answer, CheckResult, Executed, Identity, Request, Response, Task, Verdict, version,
};
use conary_github::flow::{self, Recipe};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Read;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A GitHub to ask, new for each request.
pub type GitHub = Box<dyn Fn() -> Box<dyn Api> + Send + Sync>;

pub struct Producer {
    pub cfg: Config,
    pub github: GitHub,
    pub units: Box<dyn Units>,
    signer: Signer,
    records: Records,
}

impl Producer {
    pub fn new(
        cfg: Config,
        github: GitHub,
        units: Box<dyn Units>,
        signer: Signer,
    ) -> Result<Self, String> {
        let records = Records::open(&cfg.state.join("records"))
            .map_err(|e| format!("{}: {e}", cfg.state.join("records").display()))?;
        for dir in ["git", "runs"] {
            let dir = cfg.state.join(dir);
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o750)
                .create(&dir)
                .map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        Ok(Self {
            cfg,
            github,
            units,
            signer,
            records,
        })
    }

    /// At startup: stop every run a restart left and clear every run's
    /// folder (their requests went with it), and finish what a crash left
    /// of each record: a committed one published from what it holds, any
    /// other withdrawn. How many records it finished.
    pub fn reconcile(&self) -> Result<usize, String> {
        for id in self.units.loaded()? {
            self.units.stop(&id)?;
        }
        let runs = self.cfg.state.join("runs");
        for entry in fs::read_dir(&runs).map_err(|e| format!("{}: {e}", runs.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            fs::remove_dir_all(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        publish::reconcile::<Proof>(&mut *(self.github)(), &self.records, &self.signer)
    }

    /// One request: its answer, or why there's none.
    pub fn handle(&self, request: Request, work: &Work) -> Result<Response, String> {
        if version(&request.contract).is_none_or(|v| v < 4) {
            return Err(format!(
                "{} names no commit: a proof needs conary-plugin v4",
                request.contract
            ));
        }
        let (mode, repo, head, tree, checks) = match &request.task {
            Task::Proof {
                repo,
                head,
                tree,
                checks,
            } => (Mode::Proof, repo, head, tree, checks),
            Task::Prepare {
                repo,
                head,
                tree,
                checks,
            } => (Mode::Prepare, repo, head, tree, checks),
            _ => return Err("the proof producer proves and prepares alone".into()),
        };
        if !named(repo) {
            return Err(format!("{repo:?} isn't OWNER/REPO"));
        }
        if !id(head) || !id(tree) {
            return Err("a proof names its commit and tree by their full ids".into());
        }
        if checks.is_empty() {
            return Err("a proof names its checks".into());
        }
        let recipes = self.recipes(repo, checks)?;
        let mut api = (self.github)();
        let theirs = call(
            &mut *api,
            "GET",
            &format!("repos/{repo}/git/commits/{head}"),
            None,
            None,
        )?
        .json()?;
        if theirs["tree"]["sha"].as_str() != Some(tree.as_str()) {
            return Err(format!("{tree} isn't {head}'s tree on GitHub"));
        }
        self.fetch(repo, head, tree)?;
        let answer = match mode {
            Mode::Prepare => self.prepare(repo, head, tree, &recipes, work)?,
            Mode::Proof => {
                let mut results = Vec::new();
                for (asked, (name, recipe)) in checks.iter().zip(&recipes) {
                    results
                        .push(self.prove(&mut *api, repo, head, tree, asked, name, recipe, work)?);
                }
                Answer::Proof {
                    results,
                    head: Some(head.clone()),
                }
            }
        };
        Ok(Response {
            contract: request.contract,
            by: Identity {
                engine: ENGINE.into(),
                session: None,
            },
            execution: request.execution.map(|e| Executed {
                model: None,
                effort: None,
                context: e.context,
            }),
            answer,
        })
    }

    /// Each check's recipe, as the repo's flow settings name it, read now.
    fn recipes(&self, repo: &str, checks: &[String]) -> Result<Vec<(String, Recipe)>, String> {
        let path = self.cfg.flow(repo);
        let text = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let flow: flow::Config =
            serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        checks
            .iter()
            .map(|check| match flow.settings.proof_recipe(check) {
                None => Err(format!("{check} has no recipe in the flow's settings")),
                Some((_, Recipe::Job(_))) => Err(format!(
                    "{check}'s recipe is a job, a model's work: a proof is a program"
                )),
                Some((name, recipe)) => Ok((name.clone(), recipe.clone())),
            })
            .collect()
    }

    /// The commit fetched into the repo's mirror, as `refs/proof/HEAD`,
    /// with `tree` its tree there too: as the App, whose environment the
    /// producer runs in names git's only credential helper (github.nix).
    fn fetch(&self, repo: &str, head: &str, tree: &str) -> Result<(), String> {
        let mirror = self.cfg.mirror(repo);
        let git = |args: &[&str]| -> Result<String, String> {
            let out = Command::new(&self.cfg.git)
                .args(args)
                .stdin(Stdio::null())
                .output()
                .map_err(|e| format!("git: {e}"))?;
            if !out.status.success() {
                return Err(format!(
                    "git {}: {}",
                    args.first().copied().unwrap_or_default(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        };
        let at = mirror.to_string_lossy().into_owned();
        if !mirror.is_dir() {
            git(&["init", "-q", "--bare", &at])?;
        }
        let url = format!("{}/{repo}.git", self.cfg.github.trim_end_matches('/'));
        git(&[
            "-C",
            &at,
            "fetch",
            "-q",
            "--no-tags",
            &url,
            &format!("+{head}:refs/proof/{head}"),
        ])?;
        let got = git(&[
            "-C",
            &at,
            "rev-parse",
            "--verify",
            "-q",
            &format!("{head}^{{tree}}"),
        ])?;
        if got != tree {
            return Err(format!("{head}'s tree is {got}, not {tree}"));
        }
        Ok(())
    }

    /// Run `id`'s unit, to its end, and let go of it: how it ended, and
    /// the digest of what it printed, read once nothing of it is left
    /// running. Its folder goes either way.
    fn run(&self, id: &str, spec: &Spec, work: &Work) -> Result<(Ended, String), String> {
        let ran = lay(self.cfg.bench(), &self.cfg.mirror(&spec.repo), id, spec).and_then(|()| {
            let until = Instant::now() + Duration::from_secs(self.cfg.timeout + 60);
            self.units.run(id, work, until)
        });
        let stopped = self.units.stop(id);
        let log = digest(&self.cfg.log(id));
        let _ = fs::remove_dir_all(self.cfg.run(id));
        let ended = ran?;
        stopped.map_err(|e| format!("its unit wouldn't stop: {e}"))?;
        Ok((ended, log?))
    }

    /// One check proved on `head`: its record and check run first, then
    /// the run, then its verdict signed for that commit and published.
    #[allow(clippy::too_many_arguments)]
    fn prove(
        &self,
        api: &mut dyn Api,
        repo: &str,
        head: &str,
        tree: &str,
        asked: &str,
        name: &str,
        recipe: &Recipe,
        work: &Work,
    ) -> Result<CheckResult, String> {
        let mut record = self
            .records
            .begin::<Proof>(Target {
                repo: repo.into(),
                head: head.into(),
                check: name.into(),
            })
            .map_err(|e| e.to_string())?;
        publish::start(api, &self.records, &mut record)?;
        let spec = Spec {
            mode: Mode::Proof,
            repo: repo.into(),
            head: head.into(),
            tree: tree.into(),
            check: name.into(),
            recipe: recipe.clone(),
        };
        let passed = match self.run(&run_id(), &spec, work) {
            Ok((Ended::Exited(PASSED), log)) => Ok((true, log)),
            Ok((Ended::Exited(FAILED), log)) => Ok((false, log)),
            Ok((Ended::Exited(code), _)) => Err(format!(
                "{name}: its worker exited {code}, the machine failing"
            )),
            Ok((Ended::Broke(why), _)) | Err(why) => Err(format!("{name}: {why}")),
        };
        let (passed, log) = match passed {
            Ok(got) => got,
            Err(why) => {
                publish::withdraw(api, &self.records, &mut record)?;
                return Err(why);
            }
        };
        let proof = Proof {
            repo: repo.into(),
            head: head.into(),
            tree: tree.into(),
            check: name.into(),
            recipe: recipe.clone(),
            passed,
            log,
        };
        if let Err(why) = self.records.keep(&mut record, proof) {
            publish::withdraw(api, &self.records, &mut record)?;
            return Err(why);
        }
        publish::complete(api, &self.records, &self.signer, work, &mut record)?;
        Ok(CheckResult {
            check: asked.into(),
            verdict: if passed { Verdict::Pass } else { Verdict::Fail },
            why: if passed {
                format!("{name} ran here on {head} and passed")
            } else {
                format!("{name} ran here on {head} and failed")
            },
        })
    }

    /// Whether the checks' inputs are ready, and whether proving them needs
    /// a slot: a `Build` recipe's readied in the worker's unit, a `Run`
    /// recipe's ready once the commit is fetched, and always needing one.
    fn prepare(
        &self,
        repo: &str,
        head: &str,
        tree: &str,
        recipes: &[(String, Recipe)],
        work: &Work,
    ) -> Result<Answer, String> {
        let mut slot = false;
        let mut why = Vec::new();
        for (name, recipe) in recipes {
            if !matches!(recipe, Recipe::Build(_)) {
                slot = true;
                why.push(format!("{name} runs on a fetched commit"));
                continue;
            }
            let spec = Spec {
                mode: Mode::Prepare,
                repo: repo.into(),
                head: head.into(),
                tree: tree.into(),
                check: name.clone(),
                recipe: recipe.clone(),
            };
            match self.run(&run_id(), &spec, work)?.0 {
                Ended::Exited(PASSED) => why.push(format!("{name} is built already")),
                Ended::Exited(FAILED) => {
                    slot = true;
                    why.push(format!("{name}'s inputs are built"));
                }
                Ended::Exited(code) => {
                    return Ok(not_ready(format!("{name}'s worker exited {code}")));
                }
                Ended::Broke(e) => return Ok(not_ready(format!("{name}: {e}"))),
            }
        }
        Ok(Answer::Prepare {
            ready: true,
            slot,
            why: why.join("; "),
        })
    }
}

fn not_ready(why: String) -> Answer {
    Answer::Prepare {
        ready: false,
        slot: false,
        why,
    }
}

/// OWNER/REPO, as GitHub names them.
fn named(repo: &str) -> bool {
    let mut parts = repo.split('/');
    let plain = |p: &str| {
        !p.is_empty()
            && !p.starts_with('.')
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    };
    matches!((parts.next(), parts.next(), parts.next()), (Some(o), Some(r), None) if plain(o) && plain(r))
}

/// A commit's or tree's full id, as git writes it.
fn id(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Run `id`'s folder on `cfg`, laid out for its worker, which sees nothing
/// else of the producer's: its spec, its log (empty, for the worker to
/// write), and its own copy of `mirror`, hard links of its objects, as the
/// one repo it reads. Only the producer writes the folder itself.
pub fn lay(cfg: Bench, mirror: &Path, id: &str, spec: &Spec) -> Result<(), String> {
    let dir = cfg.run(id);
    let at = |e: std::io::Error, path: &Path| format!("{}: {e}", path.display());
    fs::DirBuilder::new()
        .mode(0o750)
        .create(&dir)
        .map_err(|e| at(e, &dir))?;
    let path = dir.join(SPEC);
    fs::write(&path, serde_json::to_vec(spec).map_err(|e| e.to_string())?)
        .and_then(|()| fs::set_permissions(&path, fs::Permissions::from_mode(0o640)))
        .map_err(|e| at(e, &path))?;
    let log = cfg.log(id);
    fs::write(&log, b"")
        .and_then(|()| fs::set_permissions(&log, fs::Permissions::from_mode(0o660)))
        .map_err(|e| at(e, &log))?;
    let out = Command::new(cfg.git)
        .args(["clone", "-q", "--mirror", "--local"])
        .arg(mirror)
        .arg(cfg.repo(id))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "its copy of the mirror: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// The digest of the log at `path`, as Conary's receipt digest is
/// (`flow::receipt_digest`), however long the log.
pub fn digest(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hash = Sha256::new();
    let mut buf = vec![0; 1 << 16];
    loop {
        match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => hash.update(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
