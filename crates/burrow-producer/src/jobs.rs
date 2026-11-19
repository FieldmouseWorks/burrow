//! A producer's work run as a proj job (Conary's `docs/m6-plugins.md`, F
//! and J): started through the job runner, which keeps the slots, engines,
//! sandboxes, the acting App and usage overflow; read through its record
//! whole (`proj why JOB --json`); stopped with `proj stop`. Never `proj
//! lifecycle`, `proj state`'s snapshot or an `--owned` job: those go with
//! the steward.
//!
//! A job runs on a role from the role map, so what it runs on is the
//! role's: [`offered`] is what a plugin describes itself by, and
//! [`runnable`] holds the flow's choice to it before anything starts.
use crate::serve::Work;
use conary_core::plugin::{Choice, Model};
use serde::Deserialize;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// What a producer needs of the job runner.
pub trait Jobs: Send + Sync {
    /// A new job on `repo`'s clone for `task`, held under `key` (proj
    /// refuses a key a live job holds): its id.
    fn start(&self, repo: &str, key: &str, task: &str) -> Result<String, String>;
    /// The job holding `key`, if any.
    fn find(&self, key: &str) -> Result<Option<String>, String>;
    /// The job's record, as `proj why --json` says it.
    fn why(&self, id: &str) -> Result<Record, String>;
    /// Stop the job, queued or running, and anything it awaits.
    fn stop(&self, id: &str) -> Result<(), String>;
    /// What the job left in its answers file, if it left one.
    fn answers(&self, id: &str) -> Result<Option<Vec<u8>>, String>;
}

/// A job's record, the parts a producer reads of `proj why --json`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Record {
    /// parked, queued, running, or how its latest turn ended.
    pub stage: String,
    /// Past this, the job is a stall: the work's own deadline.
    pub overdue_at: Option<i64>,
    /// How its latest turn said it ends, a line each.
    #[serde(default)]
    pub ends: Vec<String>,
    /// Its latest turn, once it has one.
    pub turn: Option<Turn>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Turn {
    pub engine: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub session: Option<String>,
}

impl Record {
    /// Whether its turn is still to end: parked, queued or running.
    pub fn live(&self) -> bool {
        matches!(self.stage.as_str(), "parked" | "queued" | "running")
    }

    /// Whether its turn ended done, and holds nothing open: no await, no
    /// question. A producer's job is one turn.
    pub fn done(&self) -> bool {
        self.stage == "done" && self.ends.iter().all(|l| l == "done")
    }
}

/// proj, as the machine runs it, for one role's jobs.
#[derive(Debug, Clone)]
pub struct Proj {
    /// proj, and what goes before its arguments.
    pub proj: Vec<String>,
    /// proj's jobs directory, where a job leaves its answers.
    pub jobs: PathBuf,
    /// The role its jobs run as, as the role map names it.
    pub role: String,
    /// The App its jobs act on GitHub as, if the machine has one.
    pub app: Option<String>,
    /// How long a job's turn may run, and go quiet, as proj takes them.
    pub timeout: String,
    pub idle: String,
}

impl Proj {
    fn run(&self, args: &[&str]) -> Result<String, String> {
        let (program, before) = self.proj.split_first().ok_or("no proj is configured")?;
        let out = Command::new(program)
            .args(before)
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("proj doesn't start: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "proj {} failed: {}",
                args.first().unwrap_or(&""),
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    }
}

impl Jobs for Proj {
    fn start(&self, repo: &str, key: &str, task: &str) -> Result<String, String> {
        let mut args = vec!["run", "--pipeline", "--role", &self.role, "--key", key];
        if let Some(app) = &self.app {
            args.extend(["--app", app]);
        }
        args.extend(["--timeout", &self.timeout, "--idle", &self.idle, repo, task]);
        let id = self.run(&args)?;
        if id.is_empty() || id.contains(char::is_whitespace) {
            return Err(format!("proj run named no job: {id:?}"));
        }
        Ok(id)
    }

    fn find(&self, key: &str) -> Result<Option<String>, String> {
        let id = self.run(&["find", key])?;
        Ok((!id.is_empty()).then_some(id))
    }

    fn why(&self, id: &str) -> Result<Record, String> {
        let json = self.run(&["why", id, "--json"])?;
        serde_json::from_str(&json).map_err(|e| format!("proj why {id} doesn't read: {e}"))
    }

    fn stop(&self, id: &str) -> Result<(), String> {
        self.run(&["stop", id]).map(drop)
    }

    fn answers(&self, id: &str) -> Result<Option<Vec<u8>>, String> {
        let path = self.jobs.join(id).join("answers.json");
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }
}

/// Job `id`'s record once its turn has ended done, read every `look`, or
/// why it won't be: the request ended first, the job went past its own
/// deadline (the record's `overdue_at`), or its turn ended any other way.
/// The caller stops the job when this is an error.
pub fn wait(jobs: &dyn Jobs, id: &str, work: &Work, look: Duration) -> Result<Record, String> {
    let mut next = Instant::now();
    loop {
        if work.abandoned() {
            return Err(format!("the request ended before its job {id} did"));
        }
        if Instant::now() >= next {
            let record = jobs.why(id)?;
            if !record.live() {
                return if record.done() {
                    Ok(record)
                } else {
                    Err(format!(
                        "the job {id} ended {}, not done: {:?}",
                        record.stage, record.ends
                    ))
                };
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs() as i64);
            if let Some(due) = record.overdue_at
                && now > due
            {
                return Err(format!("the job {id} is overdue"));
            }
            next = Instant::now() + look;
        }
        thread::sleep(look.min(Duration::from_millis(100)));
    }
}

/// What a plugin whose work runs on a role names itself by and offers,
/// read once: the engine the role is routed to anywhere, and the models
/// that engine offers with their efforts. Conary holds every answer to
/// both, as the plugin described them, so a producer reads this once when a
/// request comes and holds the request's work to that reading
/// ([`runnable`], [`ran_as`]), whatever the settings say later.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    pub engine: String,
    pub models: Vec<Model>,
}

impl Offer {
    /// Why `model` at `effort` isn't what this offers: a model that isn't
    /// one of its models, an effort that model doesn't take, or no model
    /// when it offers any.
    fn refuses(&self, model: Option<&String>, effort: Option<&String>) -> Option<String> {
        let engine = &self.engine;
        match model {
            None if !self.models.is_empty() => Some(format!("it names no model of {engine}'s")),
            None => effort.map(|e| format!("it names an effort, {e}, but no model")),
            Some(model) => match self.models.iter().find(|m| m.id == *model) {
                None => Some(format!("{model} isn't a model the plugin offers")),
                Some(offer) => effort
                    .filter(|e| !offer.efforts.contains(e))
                    .map(|e| format!("{model} isn't offered at {e}")),
            },
        }
    }
}

/// What a plugin whose work runs on `role` names itself by and offers, as
/// the settings say now.
pub fn offered(settings: &burrow_settings::Config, role: &str) -> Result<Offer, String> {
    let settings = settings.role_view()?.in_force;
    let engine = settings.route(role, None).engine.clone();
    let offer = settings.offers.get(&engine).cloned().unwrap_or_default();
    let models = offer
        .models
        .iter()
        .map(|id| Model {
            id: id.clone(),
            efforts: offer.efforts.clone(),
            context_tokens: None,
            max_output_tokens: None,
        })
        .collect();
    Ok(Offer { engine, models })
}

/// Whether a job on `role` in `repo` would run as the flow asks, before one
/// starts: proj runs it on the role's route and tuning, and Conary refuses
/// an answer on another engine than the plugin names, a model or effort it
/// didn't offer (`offer`, as it was described), or another model or effort
/// than the flow chose. So the role's route for the repo is that engine,
/// names an offered model at an offered effort, and is what the flow chose.
pub fn runnable(
    settings: &burrow_settings::Config,
    role: &str,
    repo: &str,
    offer: &Offer,
    choice: Option<&Choice>,
) -> Result<(), String> {
    let settings = settings.role_view()?.in_force;
    let named = &offer.engine;
    let route = settings.route(role, Some(repo));
    if route.engine != *named {
        return Err(format!(
            "the {role} role can run in {repo} on another engine than {named}, which the plugin names itself by"
        ));
    }
    let tuning = settings.tuning(role, Some(repo), named);
    if let Some(why) = offer.refuses(tuning.model.as_ref(), tuning.effort.as_ref()) {
        return Err(format!(
            "the {role} role in {repo} wouldn't run as the plugin offers, so its answer would be refused: {why}"
        ));
    }
    if let Some(c) = choice
        && (c.engine != *named
            || c.model
                .as_ref()
                .is_some_and(|m| Some(m) != tuning.model.as_ref())
            || c.effort
                .as_ref()
                .is_some_and(|e| Some(e) != tuning.effort.as_ref()))
    {
        return Err(format!(
            "the flow chose {} {:?} {:?}, and the {role} role in {repo} runs {named} {:?} {:?}",
            c.engine, c.model, c.effort, tuning.model, tuning.effort
        ));
    }
    Ok(())
}

/// Whether an answer by `engine`, which reports `ran`, is what the plugin
/// offered (`offer`, as it was described, never the settings read again)
/// and what the flow chose (`choice`): its engine, a model it offers (named
/// whenever it offers any) at an effort that model takes, and exactly the
/// model and effort the flow chose, which must name the model ([`chosen`]). [`runnable`] holds the role
/// to that before a job starts; this holds what actually ran to it before
/// anything of it is published, since the role can change in between.
pub fn ran_as(
    offer: &Offer,
    choice: Option<&Choice>,
    engine: &str,
    ran: &conary_core::plugin::Executed,
) -> Result<(), String> {
    let named = &offer.engine;
    if engine != named {
        return Err(format!(
            "it ran on {engine}, and the plugin names itself by {named}"
        ));
    }
    if let Some(why) = offer.refuses(ran.model.as_ref(), ran.effort.as_ref()) {
        return Err(format!("it didn't run as the plugin offers: {why}"));
    }
    // The offer is read when the request comes, which can be after the
    // flow's describe: only what the flow chose, which Conary held to the
    // offers it negotiated, closes that gap. So the choice names a model,
    // and what ran is exactly it.
    let c = chosen(choice)?;
    if c.engine != *named || c.model != ran.model || c.effort != ran.effort {
        return Err(format!(
            "the flow chose {} {:?} {:?}, and {:?} {:?} ran",
            c.engine, c.model, c.effort, ran.model, ran.effort
        ));
    }
    Ok(())
}

/// The flow's choice, once it names the model work runs on: one that
/// doesn't leaves what runs to settings that can change after the plugin
/// described itself, which nothing here could hold to what the flow
/// negotiated.
pub fn chosen(choice: Option<&Choice>) -> Result<&Choice, String> {
    choice.filter(|c| c.model.is_some()).ok_or_else(|| {
        "the flow chose no model, so what ran couldn't be held to what it negotiated".to_string()
    })
}
