//! The review producer (Conary's `docs/m6-plugins.md`, R): a long-lived
//! service with a user of its own, which the flow's review plugin forwards
//! each review request to, from the flow's controller alone
//! (burrow-producer's socket). It holds no key: the sign pass does.
//!
//! - A request is held to GitHub before any look: its head is the PR's
//!   head, its base the tip of the PR's base branch.
//! - Its look is new work every time: a snapshot of its own, briefed with
//!   the base's rules, the PR's description as it reads now and the role's
//!   earlier findings with their replies, run in a session of its own that
//!   nothing resumes, on the model and effort the flow chose (or the
//!   role's own). Nothing is ever answered from a record.
//! - Its answer is by Codex's own session, which mustn't be the author's,
//!   and says what ran. What the look read and said is kept for the sign
//!   pass; a look abandoned or failed keeps nothing.
use crate::history::{self, Voices};
use crate::look::{Look, Looker, Looks, findings};
use crate::{Config, ENGINE, REVIEWER, digest, id, named, now};
use burrow_github::{Api, call, encode};
use burrow_producer::serve::Work;
use burrow_review::queue::{Fresh, Item};
use burrow_upkeep::snapshot::Meta;
use conary_core::plugin::{Answer, Context, Executed, Identity, Request, Response, Task, version};
use conary_github::flow::fingerprint;
use std::sync::atomic::{AtomicU64, Ordering};

/// A GitHub to ask, new for each request.
pub type GitHub = Box<dyn Fn() -> Box<dyn Api> + Send + Sync>;

/// Where a look's snapshot comes from.
pub trait Snapshots: Send + Sync {
    /// Snapshot `name` of PR `pr` at `head`, on `base_ref` whose tip is
    /// `base`: what it kept of the base. Made where the reviewers' service
    /// reads a fresh look's.
    fn take(
        &self,
        repo: &str,
        pr: u64,
        head: &str,
        base: (&str, &str),
        name: &str,
    ) -> Result<Meta, String>;
    /// Clear snapshot `name`, and what it kept.
    fn clear(&self, name: &str);
}

/// The PR as GitHub has it now.
struct Pr {
    title: String,
    body: String,
    base_ref: String,
}

pub struct Producer {
    pub cfg: Config,
    pub github: GitHub,
    pub snapshots: Box<dyn Snapshots>,
    pub looker: Box<dyn Looker>,
    /// Whom the reviewers review for (the steward's owner).
    pub owner: String,
    looks: Looks,
}

impl Producer {
    pub fn new(
        cfg: Config,
        github: GitHub,
        snapshots: Box<dyn Snapshots>,
        looker: Box<dyn Looker>,
        owner: String,
    ) -> Result<Self, String> {
        let looks = Looks::open(&cfg.state.join("looks"))?;
        Ok(Self {
            cfg,
            github,
            snapshots,
            looker,
            owner,
            looks,
        })
    }

    /// One request: its answer, or why there's none.
    pub fn handle(&self, request: Request, work: &Work) -> Result<Response, String> {
        if version(&request.contract).is_none_or(|v| v < 4) {
            return Err(format!(
                "{} isn't conary-plugin v4, which the review producer speaks",
                request.contract
            ));
        }
        let Task::Review {
            repo,
            pr,
            base,
            head,
            author,
        } = &request.task
        else {
            return Err("the review producer reviews alone".into());
        };
        if !named(repo) {
            return Err(format!("{repo:?} isn't OWNER/REPO"));
        }
        if !id(head) || !id(base) {
            return Err("a review names its head and base by their full ids".into());
        }
        let Some(execution) = &request.execution else {
            return Err("a review is asked with its execution, a fresh look".into());
        };
        if execution.context != Context::Fresh {
            return Err("a review is a fresh look: it never resumes a session".into());
        }
        let fresh = self.fresh(repo, execution.choice.as_ref())?;
        let flow = self.cfg.flow(repo)?;
        let role = &self.cfg.role;
        if flow.settings.reviewers.len() != 1 || !flow.settings.reviewers.contains(role) {
            return Err(format!(
                "the flow's reviewers in {repo} aren't {role} alone, so a request can't say whose review it is"
            ));
        }
        if flow.settings.review_authors.is_empty() {
            return Err(format!(
                "the flow in {repo} names no review accounts, so no review of its could be signed"
            ));
        }

        let mut api = (self.github)();
        let now_pr = self.held(&mut *api, repo, *pr, head, base)?;
        let tree = call(
            &mut *api,
            "GET",
            &format!("repos/{repo}/git/commits/{head}"),
            None,
            None,
        )?
        .json()?["tree"]["sha"]
            .as_str()
            .filter(|t| id(t))
            .ok_or_else(|| format!("GitHub names no tree for {head}"))?
            .to_owned();
        let voices = Voices {
            review: &flow.settings.review_authors,
            trusted: &flow.trusted_authors,
        };
        let prior = history::read(&mut *api, repo, *pr, role, &voices, None)?;
        drop(api);

        let look_id = new_id();
        let name = format!("fresh-{look_id}");
        let scope = fingerprint(&format!("{}\n{}", now_pr.title, now_pr.body));
        let ran = {
            let snapshot = Snapshot {
                snapshots: &*self.snapshots,
                name: &name,
            };
            let meta = self
                .snapshots
                .take(repo, *pr, head, (&now_pr.base_ref, base), &name)?;
            let quoted = burrow_brief::quote(&now_pr.body);
            let prompt = burrow_brief::review(&burrow_brief::Review {
                owner: &self.owner,
                repo,
                number: *pr,
                title: &now_pr.title,
                base: &now_pr.base_ref,
                quoted: &quoted,
                rules: &meta.rules,
                house: &meta.house,
                secret: &meta.secret,
                prior: &prior,
            });
            let at = now() as f64;
            let item = Item {
                reviewer: REVIEWER.into(),
                snap: name.clone(),
                repo: repo.clone(),
                number: *pr,
                head_ref_oid: head.clone(),
                base_ref_name: now_pr.base_ref.clone(),
                prompt,
                description: scope.clone(),
                opened: at,
                at,
                fresh: Some(fresh),
            };
            let ran = self.looker.look(&item, work);
            drop(snapshot);
            ran?
        };
        if work.abandoned() {
            return Err("its request was abandoned: the look keeps nothing".into());
        }
        if author.session.as_deref() == Some(ran.session.as_str()) {
            return Err(
                "the look ran in the author's own session, which can't grade its head".into(),
            );
        }
        let found = findings(&ran.answer)?;
        self.looks.keep(&Look {
            id: look_id,
            repo: repo.clone(),
            pr: *pr,
            role: role.clone(),
            head: head.clone(),
            tree,
            base: base.clone(),
            base_ref: now_pr.base_ref,
            scope,
            prior: digest(&prior),
            findings: found.clone(),
            finished: now(),
            signed: None,
        })?;
        Ok(Response {
            contract: request.contract,
            by: Identity {
                engine: ENGINE.into(),
                session: Some(ran.session),
            },
            execution: Some(Executed {
                model: ran.model,
                effort: ran.effort,
                context: Context::Fresh,
            }),
            answer: Answer::Review { findings: found },
        })
    }

    /// The model and effort a look runs on: the flow's choice, or else the
    /// role's own on Codex. One that names no model while Codex offers
    /// some couldn't say what ran, and Conary would refuse its answer.
    fn fresh(
        &self,
        repo: &str,
        choice: Option<&conary_core::plugin::Choice>,
    ) -> Result<Fresh, String> {
        if let Some(c) = choice
            && c.engine != ENGINE
        {
            return Err(format!(
                "the flow chose {}, and a look runs on {ENGINE}",
                c.engine
            ));
        }
        let settings = self.cfg.settings.role_view()?.in_force;
        let tuning = settings.tuning(&self.cfg.role, Some(repo), ENGINE);
        let offers = settings.offers.get(ENGINE).cloned().unwrap_or_default();
        let fresh = Fresh {
            model: choice.and_then(|c| c.model.clone()).or(tuning.model),
            effort: choice.and_then(|c| c.effort.clone()).or(tuning.effort),
        };
        if fresh.model.is_none() && !offers.models.is_empty() {
            return Err(format!(
                "neither the flow nor the {} role names a model on {ENGINE}, so the answer couldn't say what ran",
                self.cfg.role
            ));
        }
        Ok(fresh)
    }

    /// The PR as GitHub has it, held to the request: open, on `head`, with
    /// `base` its base branch's tip.
    fn held(
        &self,
        api: &mut dyn Api,
        repo: &str,
        pr: u64,
        head: &str,
        base: &str,
    ) -> Result<Pr, String> {
        let got = call(api, "GET", &format!("repos/{repo}/pulls/{pr}"), None, None)?.json()?;
        if got["state"].as_str() != Some("open") {
            return Err(format!("{repo}#{pr} isn't open"));
        }
        if got["head"]["sha"].as_str() != Some(head) {
            return Err(format!("{head} isn't {repo}#{pr}'s head"));
        }
        let base_ref = got["base"]["ref"]
            .as_str()
            .filter(|b| !b.is_empty() && !b.starts_with('-'))
            .ok_or_else(|| format!("{repo}#{pr} names no base branch"))?
            .to_owned();
        let tip = call(
            api,
            "GET",
            &format!("repos/{repo}/git/ref/heads/{}", encode(&base_ref)),
            None,
            None,
        )?
        .json()?;
        if tip["object"]["sha"].as_str() != Some(base) {
            return Err(format!("{base} isn't the tip of {repo}'s {base_ref}"));
        }
        Ok(Pr {
            title: got["title"].as_str().unwrap_or_default().to_owned(),
            body: got["body"].as_str().unwrap_or_default().to_owned(),
            base_ref,
        })
    }
}

/// A look's snapshot, cleared once the look is over however it ends.
struct Snapshot<'a> {
    snapshots: &'a dyn Snapshots,
    name: &'a str,
}

impl Drop for Snapshot<'_> {
    fn drop(&mut self) {
        self.snapshots.clear(self.name);
    }
}

/// A new look's id: unique on this machine, and a plain file name.
fn new_id() -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let seed = format!(
        "{nanos}-{}-{}",
        std::process::id(),
        COUNT.fetch_add(1, Ordering::SeqCst)
    );
    digest(&seed)[..16].to_owned()
}

/// Snapshots as the steward's reviewers take them (burrow-upkeep's), in
/// the producer's own state and mirrors, checked out where the reviewers'
/// service reads a fresh look's.
pub struct Upkeep {
    pub cfg: burrow_upkeep::config::Config,
    pub state: std::path::PathBuf,
}

impl Upkeep {
    /// The steward's config at `path`, its snapshots made in `fresh` and
    /// kept in `state`.
    pub fn new(
        path: &std::path::Path,
        state: &std::path::Path,
        fresh: &std::path::Path,
    ) -> Result<Self, String> {
        let mut cfg = burrow_upkeep::config::Config::read(path)?;
        cfg.snapshot.dir = fresh.to_path_buf();
        cfg.state_dir = state.to_path_buf();
        Ok(Self {
            cfg,
            state: state.to_path_buf(),
        })
    }
}

impl Snapshots for Upkeep {
    fn take(
        &self,
        repo: &str,
        pr: u64,
        head: &str,
        (base_ref, base): (&str, &str),
        name: &str,
    ) -> Result<Meta, String> {
        use burrow_upkeep::mirror::{git_in, path};
        let look = burrow_upkeep::snapshot::Look {
            number: pr,
            head: head.into(),
            base: base_ref.into(),
            name: name.into(),
        };
        if let Some(why) = look.wrong() {
            return Err(why);
        }
        let meta = burrow_upkeep::snapshot::take(&self.cfg, &self.state, repo, &look)?;
        // What it fetched is what the request named, or the base moved
        // under it: then it read another base's rules.
        let m = path(&self.state, repo);
        let at = |r: &str| {
            git_in(&m)
                .args(["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")])
                .run()
                .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
                .unwrap_or_default()
        };
        if at(&format!("refs/base/{base_ref}")) != base {
            return Err(format!(
                "{repo}'s {base_ref} moved past {base} while it was read"
            ));
        }
        if at(&format!("refs/pr/{pr}")) != head {
            return Err(format!("{repo}#{pr} moved past {head} while it was read"));
        }
        Ok(meta)
    }

    fn clear(&self, name: &str) {
        let _ = burrow_review::queue::remove(&self.cfg.snapshot.dir.join(name));
        let _ = burrow_review::queue::remove(
            &self
                .state
                .join(burrow_upkeep::snapshot::META)
                .join(format!("{name}.json")),
        );
    }
}
