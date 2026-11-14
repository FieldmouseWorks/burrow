//! Keep one entry a leg (#1011). Each push to the default branch saves its
//! leg's kept builds under a key of their own (`PREFIX` and the commit), and
//! one generation of a repo's legs comes close to its whole Actions cache.
//! Left to GitHub, the cache goes over its limit with every push, and its
//! eviction takes whatever was read longest ago: often an entry the same
//! push had just saved for another leg, while the entries it superseded,
//! which PR legs had still been restoring, stay. So the leg itself makes
//! the choice the moment it can:
//!
//!   - before its save, an entry already there for a commit that has this
//!     one in its history (a newer push that finished first) means this
//!     push's would never be restored: a restore by prefix takes the newest
//!     saved, which would be this one, older in history. It isn't saved;
//!   - after its save, once the listing shows it, every entry of the leg's
//!     whose commit is in this one's history goes, and so does its own if
//!     a newer commit's entry turned up meanwhile.
//!
//! An entry goes only when a newer commit's entry for its leg exists. One
//! whose commit is in neither's history (a branch rewritten) stays, and so
//! does every entry when the leg's own save isn't listed.

use burrow_github::{Api, call_allowing, encode, pages};
use std::collections::BTreeMap;

/// Where the leg's entries are and which is its own.
pub struct Leg<'a> {
    /// `OWNER/REPO`.
    pub repo: &'a str,
    /// The default branch, whose ref every kept entry is saved under.
    pub branch: &'a str,
    /// The key before the commit: one leg's entries, every commit's.
    pub prefix: &'a str,
    /// The commit this push checked.
    pub sha: &'a str,
}

/// How another commit's entry stands to this one's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    /// Its commit is in this one's history: superseded by this push.
    Older,
    /// This commit is in its history: it supersedes this push.
    Newer,
    /// Neither is in the other's history, or GitHub doesn't know its commit.
    Apart,
}

/// What the leg did after its save.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pruned {
    /// The entries deleted, by key.
    pub deleted: Vec<String>,
    /// Whether its own entry was among them.
    pub own: bool,
}

/// How many times the listing is read for the leg's own save before it's
/// taken as not there, and the pause between reads.
const LISTED: (u32, u64) = (12, 10);

impl Leg<'_> {
    fn reference(&self) -> String {
        format!("refs/heads/{}", self.branch)
    }

    fn own_key(&self) -> String {
        format!("{}{}", self.prefix, self.sha)
    }

    /// The leg's entries under the default branch: each commit's cache ids
    /// (a key can be saved more than once, for other paths or versions).
    fn entries(&self, api: &mut dyn Api) -> Result<BTreeMap<String, Vec<u64>>, String> {
        let path = format!(
            "repos/{}/actions/caches?key={}&ref={}&per_page=100",
            self.repo,
            encode(self.prefix),
            encode(&self.reference())
        );
        let mut entries: BTreeMap<String, Vec<u64>> = BTreeMap::new();
        for page in pages(api, &path).map_err(|e| e.to_string())? {
            for cache in page["actions_caches"].as_array().into_iter().flatten() {
                if cache["ref"] != self.reference().as_str() {
                    continue;
                }
                let Some(sha) = cache["key"]
                    .as_str()
                    .and_then(|k| k.strip_prefix(self.prefix))
                    .filter(|s| is_sha(s))
                else {
                    continue;
                };
                let id = cache["id"]
                    .as_u64()
                    .ok_or_else(|| format!("a cache entry without an id: {cache}"))?;
                entries.entry(sha.to_owned()).or_default().push(id);
            }
        }
        Ok(entries)
    }

    /// How `other`'s commit stands to this one's, by GitHub's comparison of
    /// the two.
    fn relation(&self, api: &mut dyn Api, other: &str) -> Result<Relation, String> {
        let path = format!(
            "repos/{}/compare/{}...{}?per_page=1",
            self.repo, other, self.sha
        );
        let r = call_allowing(api, "GET", &path, None, None, &[404]).map_err(|e| e.to_string())?;
        if r.status == 404 {
            return Ok(Relation::Apart);
        }
        let body = r.json().map_err(|e| e.to_string())?;
        Ok(match body["status"].as_str() {
            Some("ahead") => Relation::Older,
            Some("behind") => Relation::Newer,
            Some("diverged") => Relation::Apart,
            status => return Err(format!("GET {path}: status {status:?}")),
        })
    }

    /// Every other commit's entry, and how it stands to this one's.
    fn others(
        &self,
        api: &mut dyn Api,
        entries: &BTreeMap<String, Vec<u64>>,
    ) -> Result<Vec<(String, Relation)>, String> {
        entries
            .keys()
            .filter(|sha| *sha != self.sha)
            .map(|sha| Ok((sha.clone(), self.relation(api, sha)?)))
            .collect()
    }

    /// Before the save: a newer commit's entry for this leg, if one is
    /// there, which makes this push's save pointless.
    pub fn superseded(&self, api: &mut dyn Api) -> Result<Option<String>, String> {
        let entries = self.entries(api)?;
        Ok(self
            .others(api, &entries)?
            .into_iter()
            .find(|(_, r)| *r == Relation::Newer)
            .map(|(sha, _)| format!("{}{sha}", self.prefix)))
    }

    /// After the save: delete every entry of the leg's that a newer
    /// commit's supersedes, its own included, once its own is listed.
    pub fn prune(&self, api: &mut dyn Api) -> Result<Pruned, String> {
        let (tries, pause) = LISTED;
        let mut entries = self.entries(api)?;
        for _ in 1..tries {
            if entries.contains_key(self.sha) {
                break;
            }
            api.pause(pause);
            entries = self.entries(api)?;
        }
        if !entries.contains_key(self.sha) {
            return Err(format!(
                "{} isn't listed under {}, so nothing of this leg's was deleted",
                self.own_key(),
                self.reference()
            ));
        }
        let others = self.others(api, &entries)?;
        let newer = others.iter().any(|(_, r)| *r == Relation::Newer);
        let mut gone: Vec<&str> = others
            .iter()
            .filter(|(_, r)| *r == Relation::Older)
            .map(|(sha, _)| sha.as_str())
            .collect();
        if newer {
            gone.push(self.sha);
        }
        let mut pruned = Pruned {
            own: newer,
            ..Pruned::default()
        };
        for sha in gone {
            for id in &entries[sha] {
                let path = format!("repos/{}/actions/caches/{id}", self.repo);
                // Already gone is as good as deleted.
                call_allowing(api, "DELETE", &path, None, None, &[404])
                    .map_err(|e| e.to_string())?;
            }
            pruned.deleted.push(format!("{}{sha}", self.prefix));
        }
        Ok(pruned)
    }
}

fn is_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod tests;
