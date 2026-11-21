//! The stuck role's handoff (Conary's `docs/m6-properties.md`, SJ): what
//! the flow's `job` request asks, and what the job says it did, each held
//! to its shape before anything starts or is answered.
//!
//! The flow's `prompt` is the handoff itself, one JSON object, closed:
//!
//! ```json
//! {"pr": 7, "why": "round_limit", "detail": "what the flow saw"}
//! ```
//!
//! `why` is one of [`Stuck`]'s words, and `detail`, the flow's own account
//! (a stall's stage and the error it quoted), is required for a stall and
//! optional otherwise. `workdir` is `.`: proj picks the job's clone. The
//! answer's `summary` starts with the outcome on a line of its own
//! (`pushed SHA`, `unchanged` or `unable: WHY`), then `follow-up: URL`
//! when the job opened or reused the PR's follow-up issue, then the job's
//! own account. Its `next` is `done`: the job is one turn.
use burrow_brief::Stuck;
use serde::Deserialize;

/// The marker a PR's one follow-up issue carries in its body, by which a
/// job finds it again rather than open another.
pub fn marker(repo: &str, pr: u64) -> String {
    format!(
        "<!-- conary-stuck-followup {}#{pr} -->",
        repo.to_ascii_lowercase()
    )
}

/// The flow's handoff, from its request's `prompt`.
#[derive(Debug, Clone, PartialEq)]
pub struct Ask {
    pub pr: u64,
    pub why: Stuck,
    pub detail: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prompt {
    pr: u64,
    why: String,
    #[serde(default)]
    detail: Option<String>,
}

impl Ask {
    /// The handoff `prompt` names, in `workdir`, or why it's refused.
    pub fn read(workdir: &str, prompt: &str) -> Result<Self, String> {
        if workdir != "." {
            return Err(format!(
                "a stuck-role job runs in its repo's own clone, so its workdir is \".\", not {workdir:?}"
            ));
        }
        let p: Prompt = serde_json::from_str(prompt).map_err(|e| {
            format!("the job's prompt isn't the flow's stuck-role handoff, {{\"pr\", \"why\", \"detail\"}}: {e}")
        })?;
        if p.pr == 0 {
            return Err("a stuck-role handoff names its PR".into());
        }
        let why = Stuck::parse(&p.why).ok_or_else(|| {
            format!(
                "{:?} isn't a reason the flow hands a PR to the stuck role for: one of {:?}",
                p.why,
                Stuck::ALL.map(Stuck::word)
            )
        })?;
        let detail = match p.detail {
            Some(d) if d.trim().is_empty() => {
                return Err(
                    "a stuck-role handoff's detail, when it has one, says something".into(),
                );
            }
            d => d,
        };
        if why == Stuck::Stall && detail.is_none() {
            return Err("a stall's handoff says which stage stalled and why, in its detail".into());
        }
        Ok(Self {
            pr: p.pr,
            why,
            detail,
        })
    }
}

/// The answers file, as the brief tells the job to write it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Said {
    /// The commit its push put on the PR's branch, or null.
    pub pushed: Option<String>,
    /// Why it couldn't settle the PR, or null.
    pub unable: Option<String>,
    /// The PR's follow-up issue, by its number, or null.
    pub followup: Option<u64>,
    /// What it did, for the flow and whoever reads the job.
    pub summary: String,
}

/// What a stuck-role job did, once it holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    Pushed(String),
    Unchanged,
    Unable(String),
}

impl Said {
    pub fn read(bytes: &[u8]) -> Result<Self, String> {
        serde_json::from_slice(bytes)
            .map_err(|e| format!("the stuck-role job's answers don't read: {e}"))
    }

    /// Its outcome, if it's one. A pushed head is only what the job says:
    /// the broker holds it to GitHub's.
    pub fn outcome(&self) -> Result<Outcome, String> {
        if self.summary.trim().is_empty() {
            return Err("the stuck-role job says nothing of what it did".into());
        }
        match (&self.pushed, &self.unable) {
            (Some(_), Some(_)) => Err("the stuck-role job says it both pushed and couldn't".into()),
            (Some(head), None) if crate::answers::full_sha(head) => {
                Ok(Outcome::Pushed(head.clone()))
            }
            (Some(head), None) => Err(format!(
                "the stuck-role job's pushed head {head:?} isn't a full commit id"
            )),
            (None, Some(why)) if !why.trim().is_empty() => Ok(Outcome::Unable(why.clone())),
            (None, Some(_)) => Err("the stuck-role job says it couldn't, but not why".into()),
            (None, None) => Ok(Outcome::Unchanged),
        }
    }
}

/// The plugin's `summary`: the outcome's line, the follow-up's, then the
/// job's own.
pub fn summary(outcome: &Outcome, followup: Option<&str>, said: &str) -> String {
    let mut out = match outcome {
        Outcome::Pushed(head) => format!("pushed {head}"),
        Outcome::Unchanged => "unchanged".into(),
        Outcome::Unable(why) => format!("unable: {}", one_line(why)),
    };
    if let Some(url) = followup {
        out.push_str(&format!("\nfollow-up: {url}"));
    }
    out.push('\n');
    out.push_str(said.trim());
    out
}

fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}
