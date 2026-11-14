//! HOST-escalation: the one way anything puts the owner label on an issue or
//! PR. A producer (a job's question, the inbox sorter, the steward, the hold
//! watchdog) names a reason; [`reason::Policy`] sends it to the owner only
//! for a one-way door the machine lists, and to the operator otherwise.
//!
//! An owner escalation is one durable operation with two effects, in this
//! order, each recorded before the next:
//!
//! 1. the request goes to the owner's project through the coordinator's
//!    route ([`Project::send`]), and counts as delivered only once an
//!    acknowledger has said so ([`Project::acknowledged`], a [`Receipt`]);
//! 2. only then does the owner label go on the target ([`Labels::add`],
//!    which can't be called without that receipt).
//!
//! So no owner label stands without its delivery. A failure or a restart
//! anywhere leaves the record where it got to, and the next `escalate` or
//! `pass` carries on from there: the route finds the request it already
//! made rather than make another, and a delivered one is never sent again.

pub mod config;
pub mod reason;
pub mod relay;
mod store;

#[cfg(test)]
mod tests;

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use store::{Dir, Held, Records};

use reason::{Kind, Policy, Reason, Route};

/// What `escalate` and `pass` fail with: what went wrong, the record kept.
pub type Error = String;

/// The issue or PR that waits on the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Target {
    pub repo: String,
    pub number: u64,
}

impl Target {
    /// OWNER/NAME#N.
    pub fn parse(text: &str) -> Result<Target, Error> {
        let bad = || format!("{text:?} isn't OWNER/NAME#N");
        let (repo, n) = text.rsplit_once('#').ok_or_else(bad)?;
        let number = n.parse().ok().filter(|n| *n > 0).ok_or_else(bad)?;
        match repo.split_once('/') {
            Some((o, r))
                if !o.is_empty()
                    && !r.is_empty()
                    && !r.contains('/')
                    && !repo.contains(char::is_whitespace) =>
            {
                Ok(Target {
                    repo: repo.to_owned(),
                    number,
                })
            }
            _ => Err(bad()),
        }
    }
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}#{}", self.repo, self.number)
    }
}

/// Proof the owner's project has a request: who acknowledged it, and where.
/// Only the route makes one, from an acknowledgement it verified; nothing
/// outside this crate can, so [`Labels::add`] can't be reached without one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    by: String,
    by_id: u64,
    at: String,
    url: String,
}

impl Receipt {
    pub(crate) fn new(by: String, by_id: u64, at: String, url: String) -> Receipt {
        Receipt { by, by_id, at, url }
    }

    pub fn by(&self) -> &str {
        &self.by
    }

    pub fn by_id(&self) -> u64 {
        self.by_id
    }

    pub fn at(&self) -> &str {
        &self.at
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn json(&self) -> Value {
        json!({"by": self.by, "byId": self.by_id, "at": self.at, "url": self.url})
    }
}

/// A receipt as a record keeps it. Kept apart from [`Receipt`] so reading
/// JSON isn't a way to make one outside this crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Kept {
    by: String,
    by_id: u64,
    at: String,
    url: String,
}

impl From<&Receipt> for Kept {
    fn from(r: &Receipt) -> Kept {
        Kept {
            by: r.by.clone(),
            by_id: r.by_id,
            at: r.at.clone(),
            url: r.url.clone(),
        }
    }
}

impl From<&Kept> for Receipt {
    fn from(k: &Kept) -> Receipt {
        Receipt::new(k.by.clone(), k.by_id, k.at.clone(), k.url.clone())
    }
}

/// How far an escalation got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "camelCase", deny_unknown_fields)]
enum Stage {
    /// Kept, nothing sent yet (or sent with its answer lost).
    Recorded,
    /// The request is on the route, as `relay`; not acknowledged yet.
    Relayed { relay: u64 },
    /// Acknowledged: the owner's project has it. The label is next.
    Delivered { relay: u64, receipt: Kept },
    /// The owner label is on: done.
    Labelled { relay: u64, receipt: Kept },
}

/// One escalation, as its file keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Record {
    key: String,
    target: Target,
    kind: String,
    detail: String,
    ask: String,
    /// When it was first kept, in seconds since the epoch: the route looks
    /// for its request no further back than this.
    recorded: u64,
    stage: Stage,
}

impl Record {
    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn target(&self) -> &Target {
        &self.target
    }

    /// Whether the owner label is on.
    pub fn done(&self) -> bool {
        matches!(self.stage, Stage::Labelled { .. })
    }

    pub fn json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// A key names a file of its own: letters, digits, `.`, `_`, `-`, not
/// starting with a dot, at most 100 of them.
pub fn check_key(key: &str) -> Result<(), Error> {
    let fine = !key.is_empty()
        && key.len() <= 100
        && !key.starts_with('.')
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if fine {
        Ok(())
    } else {
        Err(format!(
            "{key:?} can't be an escalation's key (letters, digits, . _ -, not starting with .)"
        ))
    }
}

/// What the route is asked to deliver.
pub struct Request<'a> {
    pub key: &'a str,
    pub target: &'a Target,
    pub reason: &'a Reason,
    pub ask: &'a str,
    /// When its escalation was kept: a request made for it can't be older.
    pub since: u64,
}

/// The route to the owner's project, through the operator's coordinator.
pub trait Project {
    /// Puts the request on the route, once per key: a request it already
    /// made for this key (one whose answer was lost, say) is found and
    /// returned, never made again. Returns its number.
    fn send(&mut self, request: &Request) -> Result<u64, Error>;

    /// The acknowledgement that the owner's project has request `relay`,
    /// verified as from an acknowledger; None while there's none.
    fn acknowledged(&mut self, key: &str, relay: u64) -> Result<Option<Receipt>, Error>;
}

/// The owner label's one writer.
pub trait Labels {
    /// Puts `label` on the target. It takes the delivery's receipt: there's
    /// no owner label without one.
    fn add(&mut self, target: &Target, label: &str, delivered: &Receipt) -> Result<(), Error>;
}

/// What became of a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Not the owner's: the operator's, under the standing authority.
    Operator { why: String },
    /// The owner's, on its way: the label waits for the project to
    /// acknowledge the request.
    Pending { relay: Option<u64> },
    /// The project has it, and the owner label is on.
    Escalated { receipt: Receipt },
}

impl Outcome {
    pub fn json(&self) -> Value {
        match self {
            Outcome::Operator { why } => json!({"outcome": "operator", "why": why}),
            Outcome::Pending { relay } => json!({"outcome": "pending", "relay": relay}),
            Outcome::Escalated { receipt } => {
                json!({"outcome": "escalated", "receipt": receipt.json()})
            }
        }
    }
}

/// What a pass did with one escalation: its key, and its outcome.
pub type Passed = (String, Result<Outcome, Error>);

/// The policy, and what carries an escalation out.
pub struct Escalations<'a> {
    pub policy: &'a Policy,
    pub owner_label: &'a str,
    pub records: &'a mut dyn Records,
    pub project: &'a mut dyn Project,
    pub labels: &'a mut dyn Labels,
    /// Seconds since the epoch.
    pub now: u64,
}

impl Escalations<'_> {
    /// Asks for the owner on `target`, for a reason named by `word` about
    /// `detail`, with `ask` saying what they're asked. The operator gets it
    /// unless the policy lets the owner have it; then it's kept under `key`
    /// and carried as far as it will go. The same key again carries on the
    /// same escalation, never a second one.
    pub fn escalate(
        &mut self,
        key: &str,
        target: &Target,
        word: &str,
        detail: &str,
        ask: &str,
    ) -> Result<Outcome, Error> {
        check_key(key)?;
        let reason = match self.policy.route(word, detail) {
            Route::Operator(why) => return Ok(Outcome::Operator { why }),
            Route::Owner(reason) => reason,
        };
        if ask.trim().is_empty() {
            return Err("an escalation needs what the owner is asked".into());
        }
        let _held = self.records.lock()?;
        let record = match self.records.load(key)? {
            Some(kept) => {
                if kept.target != *target || kept.kind != reason.kind.word() {
                    return Err(format!(
                        "{key} is already an escalation of {} for {}, not of {target} for {}",
                        kept.target,
                        kept.kind,
                        reason.kind.word()
                    ));
                }
                kept
            }
            None => {
                let record = Record {
                    key: key.to_owned(),
                    target: target.clone(),
                    kind: reason.kind.word().to_owned(),
                    detail: reason.detail.clone(),
                    ask: ask.trim().to_owned(),
                    recorded: self.now,
                    stage: Stage::Recorded,
                };
                // Kept before anything is sent: whatever happens next, a
                // pass finds it and carries it on.
                self.records.save(&record)?;
                record
            }
        };
        self.drive(record)
    }

    /// Carries every escalation not done yet as far as it goes: what
    /// recovers one a failure or a restart left part-way.
    pub fn pass(&mut self) -> Result<Vec<Passed>, Error> {
        let _held = self.records.lock()?;
        let mut done = Vec::new();
        for key in self.records.keys()? {
            let outcome = match self.records.load(&key) {
                Ok(Some(record)) if record.done() => continue,
                Ok(Some(record)) => self.drive(record),
                Ok(None) => continue,
                Err(e) => Err(e),
            };
            done.push((key, outcome));
        }
        Ok(done)
    }

    /// One step after another, each kept before the next, until it's done
    /// or waits on the acknowledgement. Called with the records locked.
    fn drive(&mut self, mut record: Record) -> Result<Outcome, Error> {
        loop {
            match &record.stage {
                Stage::Recorded => {
                    let kind = Kind::parse(&record.kind).ok_or_else(|| {
                        format!("{}: {:?} isn't a reason", record.key, record.kind)
                    })?;
                    let reason = Reason {
                        kind,
                        detail: record.detail.clone(),
                    };
                    let relay = self.project.send(&Request {
                        key: &record.key,
                        target: &record.target,
                        reason: &reason,
                        ask: &record.ask,
                        since: record.recorded,
                    })?;
                    record.stage = Stage::Relayed { relay };
                }
                Stage::Relayed { relay } => {
                    let relay = *relay;
                    let Some(receipt) = self.project.acknowledged(&record.key, relay)? else {
                        return Ok(Outcome::Pending { relay: Some(relay) });
                    };
                    record.stage = Stage::Delivered {
                        relay,
                        receipt: Kept::from(&receipt),
                    };
                }
                Stage::Delivered { relay, receipt } => {
                    let (relay, kept) = (*relay, receipt.clone());
                    self.labels
                        .add(&record.target, self.owner_label, &Receipt::from(&kept))?;
                    record.stage = Stage::Labelled {
                        relay,
                        receipt: kept,
                    };
                }
                Stage::Labelled { receipt, .. } => {
                    return Ok(Outcome::Escalated {
                        receipt: Receipt::from(receipt),
                    });
                }
            }
            self.records.save(&record)?;
        }
    }
}
