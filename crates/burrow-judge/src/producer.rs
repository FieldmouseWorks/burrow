//! The judge producer (Conary's `docs/m6-plugins.md`, J): `autoland-judge`,
//! a long-lived service with a user of its own, which the flow's judge
//! plugin forwards each judge request to, from the flow's controller alone
//! (burrow-producer's socket). It holds the signing key and the seal key it
//! shares with the flow, each a systemd credential.
//!
//! - A request is held to GitHub before any work: the PR open on its head,
//!   with the title and description it names (`against`), and its
//!   discussion's digest, by Conary's `flow::read_judging_context`, the
//!   request's `context`. Only the flow can ask, so `why` is the flow's.
//! - It allocates the next generation for the repo and PR, past any a
//!   verdict account has posted, and keeps it before anything goes out.
//!   One persisted and never published is burned.
//! - Every request is a new judgment: the runner runs it as a fresh proj
//!   job ([`crate::runner`]). The producer refuses its answer unless its
//!   session is independent (not the author's, a reviewer's or one judged
//!   before, by Conary's own `Sessions::accept`) and what ran is what the
//!   plugin offers and the flow chose.
//! - It holds the PR to GitHub again, then keeps the judgment, commits it,
//!   posts the sealed comment (`SealKey::judgment_by`, sealing `why` and
//!   `context`) and only then starts and completes `conary/judgment` over
//!   `judgment_receipt_by(revision, generation, verdict, provenance,
//!   basis)`. Abandoned before it commits, it publishes nothing and has no
//!   check run; committed, it's published whatever happens to its request.
//! - Conary reads a head's latest `conary/judgment` run, so that run is
//!   always the highest generation's: one PR's judgments are published one
//!   at a time, one whose publication failed goes out before that PR's
//!   next, at startup if a crash came between ([`Producer::reconcile`]),
//!   and one a later generation overtook while it was judged is refused.
//!
//! The runner's answers come from a `burrow.user` proj job, which an
//! authoring session could forge: the judge stays off until #1186 moves
//! the judging engine out of that user's reach.
use crate::discussion::{self, current, highest_generation};
use crate::{Config, Read};
use burrow_github::{Api, call};
use burrow_producer::publish;
use burrow_producer::records::{Record, Records, Target};
use burrow_producer::serve::Work;
use burrow_producer::sign::{Judgment, Signer};
use conary_core::plugin::{Answer, Context, Provenance, Request, Response, Sessions, Task};
use conary_github::flow::{Basis, SealKey};
use serde_json::json;
use std::collections::HashMap;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

/// The check a judgment is published as.
pub const CHECK: &str = "conary/judgment";

/// A GitHub to write to, new for each request: the producer's App.
pub type GitHub = Box<dyn Fn() -> Box<dyn Api> + Send + Sync>;

/// Where a judgment's work is done.
pub trait Judges: Send + Sync {
    /// The judge's answer to `request`, for as long as `work` lasts.
    fn judge(&self, request: &Request, work: &Work) -> Result<Response, String>;
}

/// The runner, at its socket.
pub struct Runner(pub PathBuf);

impl Judges for Runner {
    fn judge(&self, request: &Request, work: &Work) -> Result<Response, String> {
        burrow_producer::plugin::forward(&self.0, request, work)
            .map_err(|e| format!("the runner: {e}"))
    }
}

pub struct Producer {
    pub cfg: Config,
    pub read: Read,
    pub github: GitHub,
    pub judges: Box<dyn Judges>,
    signer: Signer,
    seal: SealKey,
    records: Records,
    /// Every judge session it has accepted, so none judges twice.
    sessions: Sessions,
    /// Held while a generation is allocated.
    generations: Mutex<()>,
    /// Held while a judgment is published, with each PR's committed
    /// judgment whose publication failed, by [`pr_key`]: finished before
    /// that PR's next one goes out, so the latest `conary/judgment` run on
    /// a head is always the highest generation's, the one Conary reads.
    publishing: Mutex<HashMap<String, Record<Judgment>>>,
}

/// A PR's key among the judgments waiting to be published.
fn pr_key(repo: &str, pr: u64) -> String {
    format!("{}#{pr}", repo.to_lowercase())
}

/// The flow's seal key, from the systemd credential named `name`: 64
/// lowercase hex characters.
pub fn seal_key(name: &str) -> Result<SealKey, String> {
    let bytes = burrow_producer::credential::read(name)?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{name} isn't text"))?;
    SealKey::from_hex(text.trim()).map_err(|e| format!("{name}: {e}"))
}

/// The sealed comment judgment `j` is posted as: its reasons, with nothing
/// in them that could read as a marker, and then its issuance.
pub fn comment(seal: &SealKey, j: &Judgment) -> Result<String, String> {
    let verdict = j.verdict.ok_or("a judgment holds no verdict")?;
    let provenance = j
        .provenance
        .clone()
        .ok_or("a judgment holds no provenance")?;
    let basis = j.basis.clone().ok_or("a judgment holds no basis")?;
    let issued = seal
        .judgment_by(
            &j.repo,
            j.pr,
            verdict,
            &j.revision,
            j.generation,
            provenance,
            basis,
        )
        .map_err(str::to_owned)?;
    let reasons = j.reasons.trim().replace("<!--", "&lt;!--");
    Ok(format!("{reasons}\n\n{issued}"))
}

impl Producer {
    pub fn new(
        cfg: Config,
        read: Read,
        github: GitHub,
        judges: Box<dyn Judges>,
        signer: Signer,
        seal: SealKey,
    ) -> Result<Self, String> {
        let records = Records::open(&cfg.state.join("records")).map_err(|e| e.to_string())?;
        Ok(Self {
            cfg,
            read,
            github,
            judges,
            signer,
            seal,
            records,
            sessions: Sessions::default(),
            generations: Mutex::new(()),
            publishing: Mutex::default(),
        })
    }

    /// At startup: finish each judgment a crash interrupted. One whose
    /// comment may have gone out (committed) gets its comment, posted only
    /// if it isn't there, and then its check run; any other is withdrawn,
    /// its generation burned. Nothing is judged again. How many it
    /// finished.
    pub fn reconcile(&self) -> Result<usize, String> {
        let mut api = (self.github)();
        let seal = &self.seal;
        publish::reconcile_with::<Judgment>(
            &mut *api,
            &self.records,
            &self.signer,
            &mut |api, record| {
                let j = record
                    .data()
                    .ok_or_else(|| format!("record {} holds no judgment", record.id()))?;
                let body = comment(seal, j)?;
                if !posted(api, &j.repo, j.pr, &body)? {
                    post(api, &j.repo, j.pr, &body)?;
                }
                Ok(())
            },
        )
    }

    /// One request: its answer, or why there's none.
    pub fn handle(&self, request: Request, work: &Work) -> Result<Response, String> {
        crate::held(&request)?;
        let Task::Judge {
            repo,
            pr,
            head,
            against,
            context,
            ..
        } = &request.task
        else {
            return Err("the judge judges alone".into());
        };
        burrow_producer::jobs::runnable(
            &self.cfg.settings,
            &self.cfg.role,
            repo,
            request.execution.as_ref().and_then(|e| e.choice.as_ref()),
        )?;
        let flow = self.cfg.flow(repo)?;
        let authors = &flow.settings.verdict_authors;
        if authors.is_empty() {
            return Err(format!(
                "the flow in {repo} reads verdicts from no account, so no judgment of its would count"
            ));
        }
        if flow.settings.judge_key.as_ref() != Some(&self.seal) {
            return Err(format!(
                "the flow in {repo} doesn't read judgments under this producer's seal key"
            ));
        }
        let source = (self.read)()?;
        let (_, revision) = current(&*source, repo, *pr, head, against)?;
        if discussion::context(&*source, repo, *pr)? != *context {
            return Err(format!(
                "{repo}#{pr}'s discussion isn't the one the judgment was asked on"
            ));
        }
        let tree = discussion::tree(&*source, repo, head)?;
        let comments = source
            .comments(repo, *pr)
            .map_err(|e| format!("{repo}#{pr}'s comments: {e}"))?;
        let generation = self.allocate(repo, *pr, highest_generation(&comments, authors))?;
        drop(source);

        let mut api = (self.github)();
        let record = self
            .records
            .begin::<Judgment>(Target {
                repo: repo.clone(),
                head: head.clone(),
                check: CHECK.into(),
            })
            .map_err(|e| e.to_string())?;
        self.issue(
            &mut *api,
            &request,
            work,
            record,
            (revision, tree, generation),
        )
    }

    /// The judgment `record` is for, judged, held to GitHub again and
    /// published. Until it's committed, an error withdraws it: no check run
    /// was started for it, and its generation is burned.
    fn issue(
        &self,
        api: &mut dyn Api,
        request: &Request,
        work: &Work,
        mut record: Record<Judgment>,
        judged: (conary_github::flow::Revision, String, u64),
    ) -> Result<Response, String> {
        let (judgment, response) = match self.judge(request, work, judged) {
            Ok(done) => done,
            Err(why) => {
                let _ = publish::withdraw(api, &self.records, &mut record);
                return Err(why);
            }
        };
        let body = comment(&self.seal, &judgment)?;
        let key = pr_key(&judgment.repo, judgment.pr);
        let (repo, pr, generation) = (judgment.repo.clone(), judgment.pr, judgment.generation);
        let mut pending = self
            .publishing
            .lock()
            .map_err(|_| "the publishing lock is poisoned")?;
        // What an earlier judgment of this PR left goes out first, so this
        // one's run is the newest.
        let earlier = match pending.remove(&key) {
            Some(mut earlier) => self.finish(api, &mut earlier).inspect_err(|_| {
                pending.insert(key.clone(), earlier);
            }),
            None => Ok(()),
        };
        let superseded = earlier.and_then(|()| self.superseded(&repo, pr, generation));
        if let Err(why) = superseded {
            let _ = publish::withdraw(api, &self.records, &mut record);
            return Err(why);
        }
        if let Err(why) = self.records.keep(&mut record, judgment) {
            let _ = publish::withdraw(api, &self.records, &mut record);
            return Err(why);
        }
        if work.abandoned() {
            let _ = publish::withdraw(api, &self.records, &mut record);
            return Err("its request was abandoned: it publishes nothing".into());
        }
        // Past here what goes out stands: what fails is finished before this
        // PR's next judgment goes out, or at startup.
        self.records.commit(&mut record)?;
        let published = post(api, &repo, pr, &body)
            .and_then(|()| publish::complete(api, &self.records, &self.signer, work, &mut record));
        if let Err(why) = published {
            pending.insert(key, record);
            return Err(why);
        }
        Ok(response)
    }

    /// A committed judgment published: its comment, unless it's there, and
    /// then its check run.
    fn finish(&self, api: &mut dyn Api, record: &mut Record<Judgment>) -> Result<(), String> {
        let j = record
            .data()
            .ok_or_else(|| format!("record {} holds no judgment", record.id()))?;
        let body = comment(&self.seal, j)?;
        let (repo, pr) = (j.repo.clone(), j.pr);
        if !posted(api, &repo, pr, &body)? {
            post(api, &repo, pr, &body)?;
        }
        publish::publish(api, &self.records, &self.signer, record).map(drop)
    }

    /// Why generation `generation` would no longer be the latest on
    /// `repo`'s PR `pr`: a verdict account posted it or a later one while
    /// it was judged. Its run would then hide that one's receipt.
    fn superseded(&self, repo: &str, pr: u64, generation: u64) -> Result<(), String> {
        let flow = self.cfg.flow(repo)?;
        let source = (self.read)()?;
        let comments = source
            .comments(repo, pr)
            .map_err(|e| format!("{repo}#{pr}'s comments: {e}"))?;
        if highest_generation(&comments, &flow.settings.verdict_authors) >= generation {
            return Err(format!(
                "{repo}#{pr} has a judgment of a later generation than {generation} now, so this one is superseded"
            ));
        }
        Ok(())
    }

    /// The runner's judgment of `request`, held to the request, to the
    /// flow's choice and to GitHub again, ready to keep, and the answer
    /// the flow gets.
    fn judge(
        &self,
        request: &Request,
        work: &Work,
        (revision, tree, generation): (conary_github::flow::Revision, String, u64),
    ) -> Result<(Judgment, Response), String> {
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
        let answer = self.judges.judge(request, work)?;
        if work.abandoned() {
            return Err("its request was abandoned: it publishes nothing".into());
        }
        if answer.contract != request.contract {
            return Err("the runner answered in another contract".into());
        }
        let Answer::Judge {
            verdict,
            why: reasons,
        } = answer.answer
        else {
            return Err("the runner's answer isn't a judgment".into());
        };
        if reasons.trim().is_empty() {
            return Err("the judge gave no reasons".into());
        }
        let execution = answer
            .execution
            .filter(|e| e.context == Context::Fresh)
            .ok_or("the judge's answer doesn't say it ran fresh")?;
        if answer.by.session.as_deref() == Some(crate::FIXTURE_SESSION) {
            return Err("the judge's answer names the plugin's fixture session".into());
        }
        // Conary's own fresh-judge rule, before anything is published: never
        // the author's session, a reviewer's, or one that judged before.
        self.sessions
            .accept(&request.task, &Context::Fresh, &answer.by)
            .map_err(|e| format!("the judge's session can't judge this: {e}"))?;
        // What ran is what the plugin offers and the flow chose, before it's
        // sealed: Conary would refuse the answer, but not what went out.
        burrow_producer::jobs::ran_as(
            &self.cfg.settings,
            &self.cfg.role,
            request.execution.as_ref().and_then(|e| e.choice.as_ref()),
            &answer.by.engine,
            &execution,
        )?;
        let provenance = Provenance {
            by: answer.by.clone(),
            execution: execution.clone(),
        };
        if !provenance.valid() {
            return Err("the judge's answer doesn't say what ran, in which session".into());
        }

        // GitHub again, before anything goes out: the same head, base,
        // description and discussion as it was asked on.
        let source = (self.read)()?;
        let (_, now) = current(&*source, repo, *pr, head, against)?;
        if now != revision {
            return Err(format!("{repo}#{pr} moved while it was judged"));
        }
        if discussion::context(&*source, repo, *pr)? != *context {
            return Err(format!(
                "{repo}#{pr}'s discussion changed while it was judged"
            ));
        }
        drop(source);

        let judgment = Judgment {
            repo: repo.clone(),
            pr: *pr,
            revision,
            tree,
            generation,
            verdict: Some(verdict),
            provenance: Some(provenance),
            basis: Some(Basis {
                why: why.clone(),
                context: context.clone(),
            }),
            reasons: reasons.clone(),
        };
        let response = Response {
            contract: request.contract.clone(),
            by: answer.by,
            execution: Some(execution),
            answer: Answer::Judge {
                verdict,
                why: reasons,
            },
        };
        Ok((judgment, response))
    }

    /// The next generation for `repo`'s PR `pr`, past every one it issued
    /// and `highest`, the most any verdict account posted: kept before it's
    /// returned, so none is ever issued twice.
    fn allocate(&self, repo: &str, pr: u64, highest: u64) -> Result<u64, String> {
        let _held = self
            .generations
            .lock()
            .map_err(|_| "the generations' lock is poisoned")?;
        let dir = self.cfg.state.join("generations");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let path = dir.join(format!("{}+{pr}", repo.to_lowercase().replace('/', "+")));
        let kept = match std::fs::read_to_string(&path) {
            Ok(text) => text
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("{} isn't a generation", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let next = kept
            .max(highest)
            .checked_add(1)
            .ok_or_else(|| format!("{repo}#{pr} has no generation left"))?;
        let write = || -> std::io::Result<()> {
            let mut file = tempfile::NamedTempFile::new_in(&dir)?;
            file.write_all(next.to_string().as_bytes())?;
            file.as_file().sync_all()?;
            file.persist(&path).map_err(|e| e.error)?;
            std::fs::File::open(&dir)?.sync_all()
        };
        write().map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(next)
    }
}

/// Whether a comment saying exactly `body` is on `repo`'s PR `pr`.
fn posted(api: &mut dyn Api, repo: &str, pr: u64, body: &str) -> Result<bool, String> {
    let pages = burrow_github::pages(
        api,
        &format!("repos/{repo}/issues/{pr}/comments?per_page=100"),
    )?;
    Ok(pages
        .iter()
        .flat_map(|p| p.as_array().cloned().unwrap_or_default())
        .any(|c| c["body"].as_str() == Some(body)))
}

fn post(api: &mut dyn Api, repo: &str, pr: u64, body: &str) -> Result<(), String> {
    call(
        api,
        "POST",
        &format!("repos/{repo}/issues/{pr}/comments"),
        Some(&json!({ "body": body })),
        None,
    )
    .map(drop)
    .map_err(|e| format!("couldn't post {repo}#{pr}'s sealed comment: {e}"))
}
