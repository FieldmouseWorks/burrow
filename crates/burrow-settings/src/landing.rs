//! Who lands each repository's PRs: the steward or conaryd (Conary#313).
//!
//! Two things say it. The configured owner is a live setting, `settings
//! landing OWNER/REPO steward|conaryd`, kept in the overlay with the rest of
//! admission. The record says who holds landing now, and is written only by
//! burrow-landing, which moves a repo from one owner to the other only once
//! the one letting go has nothing left at work there: while a handover is
//! under way (`to` set) neither owner lands anything in the repo. Every
//! reader (the steward through `decision`, burrow-events' routing, conaryd's
//! events file) acts on the record, so the two never land at once.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Owner {
    #[default]
    Steward,
    Conaryd,
}

impl Owner {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "steward" => Some(Self::Steward),
            "conaryd" => Some(Self::Conaryd),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Steward => "steward",
            Self::Conaryd => "conaryd",
        }
    }
}

/// One repository's landing, as the record has it.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Holding {
    /// Who holds landing.
    pub owner: Owner,
    /// While a handover is under way, the one it goes to. Until it's done
    /// neither lands: the owner lets go of its work, and the other waits.
    pub to: Option<Owner>,
    /// Counts the repo's handovers, so a release names the one it's for.
    pub generation: u64,
    /// When the record last changed for this repo.
    #[serde(default)]
    pub since: i64,
}

impl Holding {
    /// Whether `who` may take a landing action now: it holds the repo and
    /// no handover is under way.
    pub fn lands(&self, who: Owner) -> bool {
        self.owner == who && self.to.is_none()
    }

    /// Whether `who` is letting go of the repo: it holds it, and a handover
    /// to the other is under way.
    pub fn releasing(&self, who: Owner) -> bool {
        self.owner == who && self.to.is_some_and(|to| to != who)
    }
}

/// Every repository's landing. A repo it doesn't list is the steward's.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Record {
    #[serde(default)]
    pub repos: BTreeMap<String, Holding>,
}

impl Record {
    /// The record at `path`: every repo the steward's when there's none yet.
    /// Unreadable or malformed is an error, which every reader takes as
    /// nobody may land.
    pub fn read(path: &Path) -> Result<Self, String> {
        crate::live::read(path)
    }

    /// `repo`'s holding, its name case-blind.
    pub fn holding(&self, repo: &str) -> Holding {
        self.repos
            .get(&repo.to_ascii_lowercase())
            .copied()
            .unwrap_or_default()
    }

    /// Published by an atomic, synced rename, so a reader sees the old
    /// record or the new one, whole.
    pub fn write(&self, path: &Path) -> Result<(), String> {
        let mut body = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        body.push(b'\n');
        crate::live::publish(path, &body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_is_the_stewards_and_malformed_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owners.json");
        let record = Record::read(&path).unwrap();
        let h = record.holding("Sam/Box");
        assert!(h.lands(Owner::Steward));
        assert!(!h.lands(Owner::Conaryd));
        std::fs::write(&path, b"{\"repos\": {\"sam/box\": {\"owner\": \"nobody\"}}}").unwrap();
        assert!(Record::read(&path).is_err());
    }

    #[test]
    fn a_handover_lets_neither_land() {
        let h = Holding {
            owner: Owner::Steward,
            to: Some(Owner::Conaryd),
            generation: 1,
            since: 0,
        };
        assert!(!h.lands(Owner::Steward));
        assert!(!h.lands(Owner::Conaryd));
        assert!(h.releasing(Owner::Steward));
        assert!(!h.releasing(Owner::Conaryd));
    }

    #[test]
    fn written_and_read_back_case_blind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("owners.json");
        let mut record = Record::default();
        record.repos.insert(
            "sam/box".into(),
            Holding {
                owner: Owner::Conaryd,
                to: None,
                generation: 2,
                since: 5,
            },
        );
        record.write(&path).unwrap();
        let back = Record::read(&path).unwrap();
        assert_eq!(back, record);
        assert!(back.holding("SAM/Box").lands(Owner::Conaryd));
    }
}
