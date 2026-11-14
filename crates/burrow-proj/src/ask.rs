//! Defaults and deadlines belong to the question, not to any one resume.
use crate::state_reads::write as swap;
use crate::{config::Config, job::Job, read};
use serde::{Deserialize, Serialize};
use std::fs;

mod about;
pub(crate) use about::{
    FILE as ABOUT_FILE, cohort, follows, footer, identity, key_of, load, locked, merge, open,
    restore, still_waiting,
};
mod host;
pub(crate) use host::{control_root, ensure, ensure_root, host_path, initialize};
mod visibility;
pub(crate) use visibility::tend as visibility;

pub const USAGE: &str = "usage: proj ask --default TEXT [--one-way] [--about KEY]";
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recommendation {
    pub default: String,
    pub one_way: bool,
    /// The subject: jobs asking about the same key share one notice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub about: Option<String>,
}
impl Recommendation {
    pub fn parse(args: &[String]) -> Result<Self, String> {
        let mut default = None;
        let mut one_way = false;
        let mut about = None;
        let mut words = args.iter();
        while let Some(word) = words.next() {
            match word.as_str() {
                "--default" if default.is_none() => {
                    default = words
                        .next()
                        .filter(|s| !s.trim().is_empty() && s.len() <= 16000)
                        .cloned();
                    if default.is_none() {
                        return Err(USAGE.into());
                    }
                }
                "--one-way" if !one_way => one_way = true,
                "--about" if about.is_none() => {
                    about = Some(about::normalize(words.next().ok_or(USAGE)?)?);
                }
                _ => return Err(USAGE.into()),
            }
        }
        Ok(Self {
            default: default.ok_or(USAGE)?,
            one_way,
            about,
        })
    }
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Question {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub recommendation: Recommendation,
    pub due: i64,
    /// Credits owned here while unkeyed; shared questions use the registry.
    #[serde(default)]
    pub pauses: about::Pauses,
}
const POLICY: &str = "\n\nWithout an answer by ";
const ONE_WAY: &str =
    "This is one-way: the job will finish and leave this question open for the owner.";
const REVERSIBLE: &str = "The job will take this reversible default and report the outcome.";
impl Question {
    /// The policy paragraph: rendered from the effective policy each time a
    /// notice goes out, never kept in its body.
    pub(crate) fn notice(&self) -> String {
        format!(
            "{POLICY}{}: {}. {}",
            burrow_probe::answer::utc(self.due),
            self.recommendation.default,
            if self.recommendation.one_way {
                ONE_WAY
            } else {
                REVERSIBLE
            }
        )
    }
}
/// A time as [`Question::notice`] writes it.
pub(crate) fn parse_utc(time: &str) -> Option<i64> {
    if let Some(epoch) = time.strip_prefix('@') {
        return epoch.parse().ok();
    }
    time.replacen(' ', "T", 1)
        .parse::<jiff::Timestamp>()
        .ok()
        .map(|at| at.as_second())
}
/// A notice body cached before policy was rendered at send time ends in the
/// paragraph it had then: strip that one, so the current one replaces it.
pub(crate) fn without_policy(body: &str) -> &str {
    let mut end = body.len();
    while let Some(at) = body[..end].rfind(POLICY) {
        let rest = &body[at + POLICY.len()..];
        if (rest.ends_with(ONE_WAY) || rest.ends_with(REVERSIBLE))
            && rest
                .split_once(": ")
                .is_some_and(|(time, _)| parse_utc(time).is_some())
        {
            return &body[..at];
        }
        end = at;
    }
    body
}
/// The question as the job wrote it. Use [`load`] for what it is bound by.
#[cfg(test)]
pub(crate) fn load_raw(job: &Job) -> Option<Question> {
    serde_json::from_str(&read(&host_path(job, "question"))?).ok()
}
#[derive(Serialize, Deserialize)]
struct PendingRecommendation {
    #[serde(flatten)]
    recommendation: Recommendation,
    // Older declarations were bare recommendations; turn startup already
    // clears those. New declarations can reject residue from another turn.
    #[serde(default)]
    turn: String,
}
pub(crate) fn pending(job: &Job) -> Result<Option<Recommendation>, String> {
    let state = |path: &std::path::Path| {
        crate::github_quota::state(path).map_err(|e| format!("{:?}", e.say))
    };
    if state(&job.path("end.next"))?.is_some_and(|end| end != "ask") {
        return Ok(None);
    }
    let Some(text) = state(&job.path("ask.next"))? else {
        return Ok(None);
    };
    let pending: PendingRecommendation = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if !pending.turn.is_empty() && job.turns().as_deref() != Some(pending.turn.as_str()) {
        // A recommendation left by the turn a handoff is ending is still the
        // question's until the handoff has moved it to its successor or given
        // it back: the launcher can stop between numbering the successor and
        // retargeting the record.
        let source = state(&job.path(crate::scratch::HANDOFF))?;
        if source.as_deref().map(str::trim) != Some(pending.turn.as_str()) {
            return Ok(None);
        }
    }
    Ok(Some(pending.recommendation))
}

/// Whether a one-way recommendation is active for the open question and not
/// yet committed to it: one [`pending`] still counts, so a superseded one
/// (a later `done` or `await` marker) or another turn's doesn't. A handoff
/// moves the recommendation to its successor's turn with the question, so a
/// carried one counts. Such a question is one-way already: its default never
/// runs on its own.
fn one_way_pending(job: &Job) -> Result<bool, String> {
    Ok(pending(job)?.is_some_and(|r| r.one_way))
}

/// The end marker commits the active declaration; only asks keep a pending
/// recommendation. Recovery also checks the marker if cleanup was interrupted.
pub(crate) fn declare(job: &Job, end: &str) -> Result<(), String> {
    swap(job.path("end.next"), format!("{end}\n")).map_err(|e| e.to_string())?;
    if end != "ask" {
        retire_pending(job)?;
    }
    Ok(())
}

pub(crate) fn retire_pending(job: &Job) -> Result<(), String> {
    crate::state_reads::remove_durable(job.path("ask.next")).map_err(|e| e.to_string())
}

/// Keep a superseding marker until the recommendation's absence is durable.
/// If unlink or sync fails, recovery still sees the declaration that retired it.
pub(crate) fn clear_declaration(job: &Job) -> Result<(), String> {
    retire_pending(job)?;
    crate::state_reads::remove_durable(job.path("end.next")).map_err(|e| e.to_string())
}
pub(crate) fn record(job: &Job, recommendation: &Recommendation) -> Result<(), String> {
    if recommendation.default.trim().is_empty() || recommendation.default.len() > 16000 {
        return Err(USAGE.into());
    }
    swap(
        job.path("ask.next"),
        &serde_json::to_string(&PendingRecommendation {
            recommendation: recommendation.clone(),
            turn: job.turns().unwrap_or_default(),
        })
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
pub(crate) fn finish(cfg: &Config, job: &Job, end: &str) -> Result<(), String> {
    if end == "ask" {
        ensure(job)?;
        let mut recommendation = pending(job)?.ok_or(USAGE)?;
        let prior = about::open(job)?;
        if prior.as_ref().is_some_and(|q| q.recommendation.one_way) {
            recommendation.one_way = true;
        }
        // Asking again without the key stays under the one it has.
        if recommendation.about.is_none() {
            recommendation.about = about::key_of(job)?;
        }
        let due = prior
            .as_ref()
            .map(|q| q.due)
            .unwrap_or_else(|| crate::now().saturating_add(cfg.ask_timeout as i64));

        if !host_path(job, "question-text").exists() {
            swap(
                host_path(job, "question-id"),
                crate::now_nanos().to_string(),
            )
            .map_err(|e| e.to_string())?;
            swap(host_path(job, "question-since"), crate::now().to_string())
                .map_err(|e| e.to_string())?;
            let text = job
                .turns()
                .and_then(|n| crate::state_reads::prefix(&job.turn(&n, "result"), 64000).ok())
                .map(|bytes| {
                    String::from_utf8_lossy(&bytes)
                        .trim_end_matches('\n')
                        .to_owned()
                })
                .unwrap_or_default();
            swap(
                host_path(job, "question-text"),
                burrow_probe::scrub::scrub(&text)
                    .as_str()
                    .chars()
                    .take(16000)
                    .collect::<String>(),
            )
            .map_err(|e| e.to_string())?;
        }
        let q = Question {
            pauses: prior.as_ref().map(|q| q.pauses.clone()).unwrap_or_default(),
            id: Some(
                prior
                    .and_then(|q| q.id)
                    .or_else(|| read(&host_path(job, "question-id")))
                    .ok_or("question has no identity")?,
            ),
            recommendation,
            due,
        };
        let write = |q: &Question| {
            swap(
                host_path(job, "question"),
                &serde_json::to_string(q).map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())
        };
        // A question joined to a subject takes its group's deadline and
        // requirement, and posts no notice of its own (see `about`).
        match q.recommendation.about.clone() {
            Some(key) => about::publish(job, &key, q, write)?,
            None => write(&q)?,
        }
    } else if end == "done" {
        clear(job);
    }
    retire_pending(job)?;
    Ok(())
}
pub(crate) fn clear(job: &Job) {
    for name in [
        "question",
        "question-needs-owner",
        "question-released",
        "question-text",
        "question-since",
        "question-id",
        ABOUT_FILE,
    ] {
        let _ = fs::remove_file(host_path(job, name));
    }
}

/// Called under the job lock. The wake belongs to proj-watch, including jobs
/// with no GitHub target. An unavailable starter is retried without losing intent.
pub(crate) fn expire(cfg: &Config, job: &Job) -> Result<Option<i64>, String> {
    crate::question::recover(job)?;
    if crate::question::acceptance_pending(job)? {
        return Ok(Some(crate::now() + 60));
    }
    let busy = job.end_pending() || matches!(job.state().as_str(), "queued" | "running");
    // A recommendation a handoff carried to a turn that never ran (or one left
    // for a question already open) is the question's: committed now, with the
    // question's identity, text and deadline kept, so neither its one-way flag
    // nor its default is lost to the one on record.
    if !busy && pending(job)?.is_some() {
        finish(cfg, job, "ask")?;
    }
    let Some(q) = load(job)? else { return Ok(None) };
    if crate::planning::admit(job).is_err() {
        return Ok(Some(crate::now() + 60));
    }
    if busy {
        return Ok(Some(q.due.max(crate::now() + 60)));
    }
    if crate::now() < q.due {
        return Ok(Some(q.due));
    }
    // Admission may inspect another job under its lock. Do that before
    // taking the registry, which that job may need to publish its question.
    // Keep the admission result through acceptance instead of scanning again.
    let one_way = q.recommendation.one_way || one_way_pending(job)?;
    let capacity = if one_way {
        None
    } else {
        Some(crate::inbox::resumption::admission(cfg, job)?)
    };
    // Decide and act under the registry: a join or merge that makes the group
    // one-way, or a restoration that moves its deadline, either lands first
    // and is read here, or finds this default accepted and its question retired.
    let registry = locked(cfg)?;
    let Some(q) = load(job)? else { return Ok(None) };
    if crate::now() < q.due {
        return Ok(Some(q.due));
    }
    let time = burrow_probe::answer::utc(q.due);
    if q.recommendation.one_way || one_way {
        crate::inbox::enrollment::admit(job)?;
        // A one-way decision gets no engine and cannot accidentally execute
        // its default. Record a completed host turn for the normal issue writer.
        let n = job.turns().and_then(|n| n.parse::<u64>().ok()).unwrap_or(0) + 1;
        let ntext = n.to_string();
        swap(
            host_path(job, "question-released"),
            serde_json::to_string(&q).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        swap(
            host_path(job, "question-needs-owner"),
            &q.recommendation.default,
        )
        .map_err(|e| e.to_string())?;
        let text = format!(
            "No answer by {time}. The recommended answer was: {}\n\nThis action is one-way. I left the question open for the owner under the owner label and released the job's claim.\n",
            q.recommendation.default
        );
        for (kind, value) in [
            // Durable mark: this done is the timeout's, not the job's own.
            ("timeout", "one-way\n"),
            ("result", text.as_str()),
            ("end", "done\n"),
            ("outcome", "done\n"),
        ] {
            swap(job.turn(&ntext, kind), value).map_err(|e| e.to_string())?;
        }
        swap(job.turn(&ntext, "jsonl"), serde_json::json!({"type":"result", "subtype":"success", "is_error":false, "result":text}).to_string()).map_err(|e| e.to_string())?;
        job.set_turns(n).map_err(|e| e.to_string())?;
        for path in [
            host_path(job, "question"),
            job.path("open-question"),
            job.path("await"),
        ] {
            let _ = fs::remove_file(path);
        }
        drop(registry);
        if let Some(inbox) = &cfg.inbox {
            for entry in fs::read_dir(&inbox.state).into_iter().flatten().flatten() {
                let dir = entry.path();
                if read(&dir.join("job")).as_deref() == Some(job.id) {
                    let _ = fs::remove_file(dir.join("drained"));
                }
            }
        }
        let kicks = crate::turn::lines(&job.path("kick"));
        crate::job::kick(cfg, kicks);
        crate::job::kick(cfg, vec!["autoland".into()]);
        crate::scratch::clear(cfg, job.id);
    } else {
        let prompt = format!(
            "No answer by {time}. Take your reversible recommended answer now: {}\n\nCarry out that default and say on the issue that you took it because the question timed out. Do not ask the same question again.",
            q.recommendation.default
        );
        crate::turn::start_default(
            cfg,
            job.id,
            &prompt,
            capacity
                .as_ref()
                .ok_or("one-way question lost its policy")?,
        )?;
        drop(registry);
    }
    Ok(None)
}

/// The steward's existing owner digest consumes these Rust-owned facts.
pub(crate) fn owner_lines(cfg: &Config) -> Vec<(String, String, String)> {
    let mut lines = Vec::new();
    for entry in fs::read_dir(&cfg.jobs).into_iter().flatten().flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        if id.starts_with('.') {
            continue;
        }
        let job = Job::new(cfg, &id);
        let pending = host_path(&job, "question-needs-owner").exists();
        // An unreadable registry or question lists the job: the owner hears
        // of it rather than miss a question nobody can place.
        if matches!(about::follows(&job), Ok(Some(_))) {
            continue;
        }
        let question = load(&job);
        if !pending && (host_path(&job, "question-reader").exists() || matches!(question, Ok(None)))
        {
            continue;
        }
        let link = visibility::issue(cfg, &job)
            .map(|(repo, n)| format!("https://github.com/{repo}/issues/{n}"));
        let text = read(&host_path(&job, "question-text"))
            .map(|text| summary(&text))
            .unwrap_or_default();
        let description = match question {
            Ok(Some(q)) => format!(
                "job {id} asks: {text}; default: {}; deadline {}",
                summary(&q.recommendation.default),
                burrow_probe::answer::utc(q.due)
            ),
            Err(e) if !pending => {
                format!("job {id} asks: {text}; its deadline and default can't be read: {e}")
            }
            _ => format!(
                "job {id}: one-way question remains open: {text}; recommended {}",
                summary(&read(&host_path(&job, "question-needs-owner")).unwrap_or_default())
            ),
        };
        let others: Vec<String> = cohort(&job)
            .unwrap_or_default()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let description = if others.is_empty() {
            description
        } else {
            format!("{description}; also waiting on it: {}", others.join(", "))
        };
        let line = link.map(|url| format!("[{id}]({url})")).unwrap_or(id);
        lines.push(("Jobs waiting for the owner".into(), line, description));
    }
    lines.sort();
    lines
}

fn summary(text: &str) -> String {
    burrow_probe::scrub::scrub(text)
        .as_str()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(400)
        .collect()
}

#[cfg(test)]
#[path = "ask/tests/about.rs"]
pub(crate) mod about_tests;
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        cfg.scratch = dir.path().join("scratch");
        cfg.tools.systemctl = dir.path().join("systemctl");
        burrow_test::executable(
            &cfg.tools.systemctl,
            "#!/bin/sh\ncase \"$*\" in *is-active*) exit 3 ;; *LoadState*) echo not-found ;; esac\n",
        );
        cfg.tools.systemd_run = "true".into();
        cfg.kick = "http://127.0.0.1:1/kick".into();
        (dir, cfg)
    }
    fn asking(cfg: &Config, one_way: bool) -> Job<'_> {
        let job = Job::new(cfg, "j1");
        fs::create_dir_all(&job.dir).unwrap();
        crate::policy::bind(
            &job,
            crate::policy::Identity::Local,
            crate::policy::Kind::Manual,
        )
        .unwrap();
        ensure(&job).unwrap();
        job.set_turns(1).unwrap();
        fs::write(job.path("session"), "s1\n").unwrap();
        fs::write(job.turn("1", "end"), "ask\n").unwrap();
        fs::write(job.turn("1", "outcome"), "done\n").unwrap();
        let q = Question {
            pauses: Default::default(),
            id: None,
            recommendation: Recommendation {
                default: "keep the change".into(),
                one_way,
                about: None,
            },
            due: crate::now() - 1,
        };
        fs::write(
            host_path(&job, "question"),
            serde_json::to_string(&q).unwrap(),
        )
        .unwrap();
        job
    }
    #[test]
    fn ask_requires_a_nonempty_default_in_both_clients() {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            vec!["ask".into()],
            vec!["ask".into(), "--one-way".into()],
            vec!["ask".into(), "--default".into(), " ".into()],
        ] {
            assert!(crate::guest::declare(&dir.path().join("next"), &args).is_err());
            assert!(Recommendation::parse(&args[1..]).is_err());
        }
        assert!(Recommendation::parse(&["--default".into(), "continue".into()]).is_ok());
    }
    #[test]
    fn expired_reversible_question_resumes_with_its_default() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        let _held = job.lock().unwrap();
        assert_eq!(expire(&cfg, &job).unwrap(), None);
        assert_eq!(job.turns().as_deref(), Some("2"));
        let prompt = read(&job.path("prompt.2")).unwrap();
        assert!(prompt.contains("No answer by"));
        assert!(prompt.contains("Take your reversible recommended answer now: keep the change"));
        assert!(!prompt.contains("question is still posted"));
        assert!(load(&job).unwrap().is_none());
    }
    #[test]
    fn split_intent_defers_both_question_expiry_transitions_without_losing_state() {
        for one_way in [false, true] {
            let (_dir, cfg) = fixture();
            let job = asking(&cfg, one_way);
            fs::write(job.turn("1", "split"), "[]").unwrap();
            let before = fs::read(host_path(&job, "question")).unwrap();
            let _held = job.lock().unwrap();
            assert!(expire(&cfg, &job).unwrap().is_some());
            assert_eq!(job.turns().as_deref(), Some("1"));
            assert_eq!(fs::read(host_path(&job, "question")).unwrap(), before);
            assert!(!job.turn("2", "end").exists());
            // Even a legacy synthetic turn cannot hide an earlier intent.
            job.set_turns(2).unwrap();
            assert_eq!(crate::planning::pending_split(&job).as_deref(), Some("1"));
            assert!(crate::planning::admit(&job).is_err());
        }
    }
    #[test]
    fn split_holds_without_question_work_do_not_schedule_wakes() {
        for state in ["held", "retired", "closed", "unenrolled"] {
            let (_dir, cfg) = fixture();
            let job = asking(&cfg, true);
            fs::write(job.turn("1", "split"), "[]").unwrap();
            fs::write(job.path("planning-hold"), "1").unwrap();
            if state == "held" {
                clear(&job);
            } else {
                crate::cmd::stop(&cfg, "j1", false).unwrap();
                assert!(job.turn("1", "stopped").exists());
            }
            let _held = job.lock().unwrap();
            assert_eq!(expire(&cfg, &job).unwrap(), None, "{state}");
            assert_eq!(job.turns().as_deref(), Some("1"));
        }
    }
    #[test]
    fn expired_one_way_question_completes_without_an_engine_and_stays_visible() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, true);
        let _held = job.lock().unwrap();
        expire(&cfg, &job).unwrap();
        drop(_held);
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert_eq!(read(&job.turn("2", "end")).as_deref(), Some("done"));
        assert!(!job.path("prompt.2").exists());
        assert!(
            crate::cmd::told(&cfg, "j1", "2")
                .unwrap()
                .contains("one-way")
        );
        assert!(host_path(&job, "question-needs-owner").exists());
        assert!(load(&job).unwrap().is_none());
        assert_eq!(owner_lines(&cfg).len(), 1);
    }
    // A one-way upgrade left for a reversible question (a handoff carries it
    // to a turn that may never run) is no reversible default's to override.
    #[test]
    fn a_pending_one_way_upgrade_stops_a_reversible_default_from_running() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        fs::write(
            job.path("ask.next"),
            r#"{"default":"delete it","one_way":true,"turn":"1"}"#,
        )
        .unwrap();
        let _held = job.lock().unwrap();
        expire(&cfg, &job).unwrap();
        drop(_held);
        assert!(!job.path("prompt.2").exists(), "no engine ran the default");
        assert_eq!(read(&job.turn("2", "end")).as_deref(), Some("done"));
        assert!(host_path(&job, "question-needs-owner").exists());
    }
    // A carried one-way recommendation with no committed question yet (the
    // successor failed before it could commit one) is published at expiry with
    // its own default, and an upgrade over a reversible question brings its
    // default along, not the old one.
    #[test]
    fn a_carried_recommendation_is_committed_whole_when_the_successor_never_runs() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        fs::write(
            job.path("ask.next"),
            r#"{"default":"delete it","one_way":true,"turn":"1"}"#,
        )
        .unwrap();
        let _held = job.lock().unwrap();
        expire(&cfg, &job).unwrap();
        drop(_held);
        assert_eq!(
            read(&host_path(&job, "question-needs-owner")).as_deref(),
            Some("delete it")
        );
        // No committed question at all.
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        fs::remove_file(host_path(&job, "question")).unwrap();
        fs::write(
            job.path("ask.next"),
            r#"{"default":"delete it","one_way":true,"turn":"1"}"#,
        )
        .unwrap();
        let _held = job.lock().unwrap();
        assert!(expire(&cfg, &job).unwrap().is_some());
        let q = load(&job).unwrap().unwrap();
        assert!(q.recommendation.one_way);
        assert_eq!(q.recommendation.default, "delete it");
    }
    // The launcher of a handoff can die after numbering the successor and
    // before retargeting the recommendation: it is still the question's, for
    // the handoff's source turn only, until the handoff is over.
    #[test]
    fn a_handoff_in_progress_keeps_its_source_turns_recommendation() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        fs::write(
            job.path("ask.next"),
            r#"{"default":"delete it","one_way":true,"turn":"1"}"#,
        )
        .unwrap();
        job.set_turns(2).unwrap();
        // No handoff: the turn count moved on, so it is another turn's.
        assert!(!one_way_pending(&job).unwrap());
        fs::write(job.path(crate::scratch::HANDOFF), "1\n").unwrap();
        assert!(one_way_pending(&job).unwrap());
        let kept = pending(&job).unwrap().unwrap();
        assert_eq!(kept.default, "delete it");
        // Another turn's still isn't.
        fs::write(
            job.path("ask.next"),
            r#"{"default":"x","one_way":true,"turn":"7"}"#,
        )
        .unwrap();
        assert!(!one_way_pending(&job).unwrap());
    }
    // A one-way recommendation that was superseded (a later marker committed
    // before its removal failed) or belongs to another turn doesn't make the
    // question one-way.
    #[test]
    fn a_superseded_or_foreign_one_way_recommendation_does_not_hold_the_question() {
        for (end, turn) in [(Some("done"), "1"), (None, "7")] {
            let (_dir, cfg) = fixture();
            let job = asking(&cfg, false);
            if let Some(end) = end {
                fs::write(job.path("end.next"), format!("{end}\n")).unwrap();
            }
            fs::write(
                job.path("ask.next"),
                format!(r#"{{"default":"delete it","one_way":true,"turn":"{turn}"}}"#),
            )
            .unwrap();
            assert!(!one_way_pending(&job).unwrap());
        }
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        fs::write(
            job.path("ask.next"),
            r#"{"default":"delete it","one_way":true,"turn":"1"}"#,
        )
        .unwrap();
        assert!(one_way_pending(&job).unwrap());
    }
    // What a job still holds open (Job::holds), through each end its
    // question can come to: the steward and claims take one that holds
    // anything for one still at work, never as ended (remi#881).
    #[test]
    fn a_question_is_held_until_its_job_resumes_or_stops() {
        for one_way in [false, true] {
            let (_dir, cfg) = fixture();
            let job = asking(&cfg, one_way);
            fs::write(job.path("await"), "sam/up#3\t0\t{}\n").unwrap();
            assert_eq!(job.holds().unwrap(), ["ask", "await sam/up#3"]);
            fs::remove_file(job.path("await")).unwrap();
            let held = job.lock().unwrap();
            expire(&cfg, &job).unwrap();
            drop(held);
            if one_way {
                // Its synthetic turn is done, but the owner's answer is owed.
                assert_eq!(read(&job.turn("2", "end")).as_deref(), Some("done"));
                assert_eq!(job.holds().unwrap(), ["owner"]);
            } else {
                // Resumed with its default: the question's gone, and the
                // turn it started is what says it's at work now.
                assert_eq!(job.turns().as_deref(), Some("2"));
                assert!(
                    job.holds().unwrap().is_empty(),
                    "{:?}",
                    job.holds().unwrap()
                );
            }
            crate::cmd::halt(&cfg, &job, false).unwrap();
            assert!(
                job.holds().unwrap().is_empty(),
                "proj stop leaves nothing held"
            );
        }
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        fs::write(job.turn("1", "end"), "done\n").unwrap();
        clear(&job);
        assert!(job.holds().unwrap().is_empty(), "a done turn holds nothing");
        // A resume that failed leaves its question, which its deadline
        // resumes: held whatever the latest outcome.
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        job.set_turns(2).unwrap();
        fs::write(job.turn("2", "outcome"), "failed\n").unwrap();
        assert_eq!(
            job.lifecycle().unwrap(),
            ("failed".to_owned(), vec!["ask".to_owned()])
        );
        // A record it can't read is an error, never nothing held.
        fs::create_dir(job.path("await")).unwrap();
        assert!(job.holds().is_err() && job.lifecycle().is_err());
    }

    // State and holds are one snapshot: no turn starts between them, since
    // lifecycle waits out a start holding the turn-start lock.
    #[test]
    fn lifecycle_reads_under_the_turn_start_lock() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        let start = job.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            s.spawn(|| tx.send(Job::new(&cfg, "j1").lifecycle()).unwrap());
            assert!(
                rx.recv_timeout(std::time::Duration::from_millis(300))
                    .is_err()
            );
            // The start publishes its turn and retires the question.
            job.set_turns(2).unwrap();
            fs::remove_file(host_path(&job, "question")).unwrap();
            drop(start);
            let (_, held) = rx.recv().unwrap().unwrap();
            assert!(held.is_empty(), "{held:?}");
        });
    }
    #[test]
    fn a_timed_out_one_way_question_requires_a_person_before_resuming() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, true);
        fs::remove_file(host_path(&job, "question")).unwrap();
        fs::write(host_path(&job, "question-needs-owner"), "keep the change").unwrap();
        fs::write(job.turn("1", "end"), "done\n").unwrap();
        assert!(crate::turn::start(&cfg, "j1", "machine news", false).is_err());
        assert!(host_path(&job, "question-needs-owner").exists());
        crate::turn::start_answer(&cfg, "j1", "the owner answered", false).unwrap();
        assert!(!host_path(&job, "question-needs-owner").exists());
    }
    #[test]
    fn repeating_a_question_cannot_drop_its_one_way_requirement() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, true);
        let due = load(&job).unwrap().unwrap().due;
        record(
            &job,
            &Recommendation {
                default: "keep it".into(),
                one_way: false,
                about: None,
            },
        )
        .unwrap();
        finish(&cfg, &job, "ask").unwrap();
        let question = load(&job).unwrap().unwrap();
        assert!(question.recommendation.one_way);
        assert_eq!(question.due, due);
    }
    #[test]
    fn writable_stop_markers_and_turn_selectors_do_not_cancel_question_authority() {
        for n in ["1", "9"] {
            let (_dir, cfg) = fixture();
            let job = asking(&cfg, true);
            let mut q = load(&job).unwrap().unwrap();
            q.due = crate::now() + 3600;
            fs::write(
                host_path(&job, "question"),
                serde_json::to_string(&q).unwrap(),
            )
            .unwrap();
            fs::write(job.turn("1", "stopped"), "").unwrap();
            job.set_turns(n.parse::<u64>().unwrap()).unwrap();
            fs::write(job.turn(n, "stopped"), "").unwrap();
            assert_eq!(expire(&cfg, &job).unwrap(), Some(q.due));
            crate::question::Resume::prepare(&job, false)
                .unwrap()
                .commit()
                .unwrap();
            record(
                &job,
                &Recommendation {
                    default: "keep it".into(),
                    one_way: false,
                    about: None,
                },
            )
            .unwrap();
            finish(&cfg, &job, "ask").unwrap();
            assert!(load(&job).unwrap().unwrap().recommendation.one_way);
            assert_eq!(load(&job).unwrap().unwrap().due, q.due);
            crate::cmd::stop(&cfg, "j1", false).unwrap();
            assert!(
                load(&job).unwrap().is_none(),
                "authenticated operator stop clears the question"
            );
        }
    }
    #[test]
    fn original_question_survives_unrelated_and_repeated_ask_turns_and_timeout() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, true);
        fs::remove_file(host_path(&job, "question")).unwrap();
        fs::write(job.turn("1", "result"), "Which release should run?").unwrap();
        let recommendation = Recommendation {
            default: "keep it".into(),
            one_way: true,
            about: None,
        };
        record(&job, &recommendation).unwrap();
        finish(&cfg, &job, "ask").unwrap();
        for n in 2..5 {
            job.set_turns(n).unwrap();
            assert!(owner_lines(&cfg)[0].2.contains("Which release should run?"));
            fs::write(
                job.turn(&n.to_string(), "result"),
                "The unrelated update is handled.",
            )
            .unwrap();
            record(&job, &recommendation).unwrap();
            finish(&cfg, &job, "ask").unwrap();
            assert!(owner_lines(&cfg)[0].2.contains("Which release should run?"));
        }
        let mut q = load(&job).unwrap().unwrap();
        q.due = crate::now() - 1;
        fs::write(
            host_path(&job, "question"),
            serde_json::to_string(&q).unwrap(),
        )
        .unwrap();
        expire(&cfg, &job).unwrap();
        assert!(owner_lines(&cfg)[0].2.contains("Which release should run?"));
        crate::question::Resume::prepare(&job, true)
            .unwrap()
            .commit()
            .unwrap();
        assert!(!host_path(&job, "question-text").exists());
    }
    #[test]
    fn failed_timeout_start_keeps_the_default_and_deadline() {
        let (_dir, mut cfg) = fixture();
        cfg.tools.systemd_run = "false".into();
        let job = asking(&cfg, false);
        let before = read(&host_path(&job, "question"));
        assert!(expire(&cfg, &job).is_err());
        assert_eq!(read(&host_path(&job, "question")), before);
    }
    #[test]
    fn unrelated_resume_preserves_deadline_but_human_reply_clears_it() {
        let (_dir, cfg) = fixture();
        let job = asking(&cfg, false);
        let before = read(&host_path(&job, "question"));
        crate::question::Resume::prepare(&job, false)
            .unwrap()
            .commit()
            .unwrap();
        assert_eq!(read(&host_path(&job, "question")), before);
        {
            let _resume = crate::question::Resume::prepare(&job, true).unwrap();
            assert_eq!(read(&host_path(&job, "question")), before);
        }
        assert_eq!(read(&host_path(&job, "question")), before);
        crate::question::Resume::prepare(&job, true)
            .unwrap()
            .commit()
            .unwrap();
        assert!(load(&job).unwrap().is_none());
    }
}
