//! A major's verdicts: sealed as the one who lands the repo reads them
//! (the steward v1, conaryd v2: seal.rs), and read back as it does.

use burrow_settings::landing::{Owner, Record};
use conary_github::flow::Revision;

use super::{Broke, Config, Sweep, seal};
use crate::complain;

/// A major as it's handed over: its number, head and base, and in a repo
/// conaryd lands, what its v2 verdicts seal.
pub(super) struct Major {
    pub n: String,
    pub sha: String,
    pub base: String,
    pub v2: Option<V2>,
}

/// What a major's v2 verdicts seal, and the highest generation already
/// sealed on it.
pub(super) struct V2 {
    pub revision: Revision,
    pub posted: u64,
}

impl Config {
    /// Its seal key made, if it has none or it isn't one, as a seal would
    /// make it: what it exits with.
    pub fn key(&self) -> u8 {
        match seal::ensure(&self.state) {
            Ok(()) => 0,
            Err(why) => {
                complain(&format!("{}: {why}", self.name));
                1
            }
        }
    }

    /// The markers that seal major `m`'s ok and held verdicts, in `repo`:
    /// as conaryd reads them where it lands, else as the steward does.
    pub(super) fn seal(&self, repo: &str, m: &Major) -> Result<[String; 2], Broke> {
        let broke = |why| Broke(format!("{repo}: sealing #{}: {why}", m.n));
        if let Some(v2) = &m.v2 {
            let n =
                m.n.parse()
                    .map_err(|_| broke(format!("{:?} isn't a number", m.n)))?;
            let issuance = seal::Issuance {
                repo,
                n,
                revision: &v2.revision,
            };
            return issuance.markers(&self.state, v2.posted).map_err(broke);
        }
        let [ok, held] = ["ok", "held"].map(|v| {
            seal::Verdict {
                repo,
                n: &m.n,
                verdict: v,
                head: &m.sha,
                base: &m.base,
            }
            .marker(&self.state)
        });
        Ok([ok.map_err(broke)?, held.map_err(broke)?])
    }

    /// Whether burrow-landing's record gives `repo` to conaryd, or is
    /// handing it over to it: never with no record to read.
    pub(super) fn conaryd(&self, repo: &str) -> Result<bool, Broke> {
        let Some(path) = &self.landing else {
            return Ok(false);
        };
        let holding = Record::read(path).map_err(Broke)?.holding(repo);
        Ok(holding.to.unwrap_or(holding.owner) == Owner::Conaryd)
    }
}

impl Sweep<'_> {
    /// What PR `n`'s v2 `payloads` say of `revision`, under the sweep's key.
    pub(super) fn issued(&self, n: &str, revision: &Revision, payloads: &[String]) -> seal::Read {
        let Ok(n_) = n.parse() else {
            return seal::Read::default();
        };
        let issuance = seal::Issuance {
            repo: self.repo,
            n: n_,
            revision,
        };
        issuance
            .read(&self.cfg.state, payloads)
            .unwrap_or_else(|why| {
                complain(&format!(
                    "{}: {}: checking #{n}'s seal: {why}",
                    self.cfg.name, self.repo
                ));
                seal::Read::default()
            })
    }

    /// Whether `mac` seals verdict `v` on PR `n`'s `sha` for `base`.
    pub(super) fn sealed(&self, n: &str, v: &str, sha: &str, base: &str, mac: &str) -> bool {
        let verdict = seal::Verdict {
            repo: self.repo,
            n,
            verdict: v,
            head: sha,
            base,
        };
        verdict.sealed(&self.cfg.state, mac).unwrap_or_else(|why| {
            complain(&format!(
                "{}: {}: checking #{n}'s seal: {why}",
                self.cfg.name, self.repo
            ));
            false
        })
    }
}
