//! A major's verdict in a repo the landing record gives conaryd: Conary's
//! v2 issuance (`SealKey::decision`), the only form its flow reads, under
//! the same key as v1 (the flow's `major_key`, autoland/shadow.nix). It
//! seals the PR's revision, its description included as Conary's read
//! sees it, and a generation: the latest generation verified on the
//! revision is the verdict, so each is allocated past every one before it,
//! and kept, before a marker carrying it is handed out.
//!
//! `<!-- major-verdict:v2 {"issued":{…},"tag":"…"} -->`, alone on its line.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use conary_github::flow::Revision;
use serde_json::Value;
use subtle::ConstantTimeEq;

use super::{KEY, NAME, key};

/// One major's revision, in `repo` (OWNER/NAME), as PR `n`.
pub(crate) struct Issuance<'a> {
    pub repo: &'a str,
    pub n: u64,
    pub revision: &'a Revision,
}

/// What a PR's v2 markers say, as Conary reads them.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Read {
    /// The verdict that counts on the revision: the latest generation's,
    /// when it says one thing and that's ok or held.
    pub verdict: Option<String>,
    /// The highest generation sealed under the key, on any revision.
    pub highest: u64,
}

impl Issuance<'_> {
    /// The ok and held markers, sealed under the key in `state` (made if
    /// there's none), each on a generation of its own past `posted` and
    /// every one allocated here before, held's the later: a job that posts
    /// both is held. Both generations are kept before either is returned.
    pub(crate) fn markers(&self, state: &Path, posted: u64) -> Result<[String; 2], String> {
        let path = state.join(KEY);
        let key = key(&path, true)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .expect("a key it made");
        let first = self.allocate(state, posted, 2)?;
        Ok(["ok", "held"].map(|verdict| {
            let generation = first + u64::from(verdict == "held");
            key.decision(self.repo, self.n, NAME, verdict, self.revision, generation)
        }))
    }

    /// What `payloads` (each the text between `<!-- major-verdict:v2 ` and
    /// ` -->` on a line of its own, oldest first) say under the key in
    /// `state`: nothing with no key there (it makes none). Each counts only
    /// as the sweep sealed it, byte for byte.
    pub(crate) fn read(&self, state: &Path, payloads: &[String]) -> Result<Read, String> {
        let path = state.join(KEY);
        let Some(key) = key(&path, false).map_err(|e| format!("{}: {e}", path.display()))? else {
            return Ok(Read::default());
        };
        let mut read = Read::default();
        let mut words = BTreeMap::<u64, BTreeSet<String>>::new();
        for payload in payloads {
            let Some((generation, verdict, revision)) = issued(payload) else {
                continue;
            };
            if generation == 0 {
                continue;
            }
            let want = key.decision(self.repo, self.n, NAME, &verdict, &revision, generation);
            let got = format!("<!-- {NAME}:v2 {payload} -->");
            if !bool::from(want.as_bytes().ct_eq(got.as_bytes())) {
                continue;
            }
            read.highest = read.highest.max(generation);
            if &revision == self.revision {
                words.entry(generation).or_default().insert(verdict);
            }
        }
        read.verdict = words
            .into_iter()
            .next_back()
            .filter(|(_, said)| said.len() == 1)
            .and_then(|(_, said)| said.into_iter().next())
            .filter(|v| matches!(v.as_str(), "ok" | "held"));
        Ok(read)
    }

    /// The first of `count` generations past `posted` and the last kept
    /// in `state`, all of them kept, synced, before it returns.
    fn allocate(&self, state: &Path, posted: u64, count: u64) -> Result<u64, String> {
        let path = generations(state, self.repo, self.n);
        let shown = |e: std::io::Error| format!("{}: {e}", path.display());
        let kept = match fs::read_to_string(&path) {
            Ok(text) => text
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("{} isn't a generation", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
            Err(e) => return Err(shown(e)),
        };
        let first = kept
            .max(posted)
            .checked_add(1)
            .filter(|f| f.checked_add(count - 1).is_some())
            .ok_or_else(|| format!("{}: no generation left", path.display()))?;
        let last = first + count - 1;
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(format!(".{}.new", std::process::id()));
        let tmp = PathBuf::from(tmp);
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(shown)?;
        f.write_all(format!("{last}\n").as_bytes())
            .and_then(|()| f.sync_all())
            .map_err(shown)?;
        drop(f);
        fs::rename(&tmp, &path).map_err(shown)?;
        File::open(state)
            .and_then(|d| d.sync_all())
            .map_err(shown)?;
        Ok(first)
    }
}

/// Where the last generation allocated on PR `n` of `repo` is kept: the
/// repo in lower case, as GitHub doesn't mind it.
fn generations(state: &Path, repo: &str, n: u64) -> PathBuf {
    state.join(format!(
        "{}-{n}.major-generation",
        repo.to_lowercase().replace('/', "_")
    ))
}

/// A marker's generation, verdict and revision, if it names them.
fn issued(payload: &str) -> Option<(u64, String, Revision)> {
    let json: Value = serde_json::from_str(payload).ok()?;
    let issued = json.get("issued")?;
    let revision = serde_json::from_value(issued.get("revision")?.clone()).ok()?;
    Some((
        issued.get("generation")?.as_u64()?,
        issued.get("verdict")?.as_str()?.to_owned(),
        revision,
    ))
}

#[cfg(test)]
mod tests;
