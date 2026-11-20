//! A job's verdict on a major, sealed: a MAC under a key in the sweep's
//! state dir, which only the sweep holds. Every Claude job posts as the same
//! login, so without it a job quoting PR text that holds a well-formed
//! marker would pass for the verdict. The MAC is Conary's `SealKey::seal`,
//! the one the autoland steward (modules/autoland/seal.py) and conaryd check
//! a major's verdict with, so the marker is the v1 one they read:
//! `<!-- major-verdict: ok|held SHA on BASE MAC -->`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use conary_github::flow::SealKey;
use regex::Regex;
use subtle::ConstantTimeEq;

use crate::say;

/// What a major's marker is called, and what its MAC seals it as.
const NAME: &str = "major-verdict";
/// The key, in the sweep's state dir.
const KEY: &str = "verdict.key";

static REPO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[^\s/]+/[^\s/]+\z").expect("a regex"));
static NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[1-9][0-9]*\z").expect("a regex"));
static HEAD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[0-9a-f]{40}\z").expect("a regex"));
static BASE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[^\s>]+\z").expect("a regex"));

/// Verdict `verdict` (ok or held) on PR `n` of `repo` (OWNER/NAME), at head
/// `head` for base `base`.
pub(crate) struct Verdict<'a> {
    pub repo: &'a str,
    pub n: &'a str,
    pub verdict: &'a str,
    pub head: &'a str,
    pub base: &'a str,
}

impl Verdict<'_> {
    /// The marker a job ends its comment with for this verdict, sealed
    /// under the key in `state`, made if there's none or it isn't one. It
    /// seals only what a marker can name and the sweep can read back.
    pub(crate) fn marker(&self, state: &Path) -> Result<String, String> {
        let Self {
            repo,
            n,
            verdict,
            head,
            base,
        } = self;
        if !REPO.is_match(repo) {
            return Err(format!("the repo is OWNER/NAME, not {repo:?}"));
        }
        if !NUMBER.is_match(n) {
            return Err(format!("a PR's number, not {n:?}"));
        }
        if !matches!(*verdict, "ok" | "held") {
            return Err(format!("a verdict is ok or held, not {verdict:?}"));
        }
        if !HEAD.is_match(head) {
            return Err(format!("the PR's full head commit, not {head:?}"));
        }
        if !BASE.is_match(base) {
            return Err(format!("the branch the PR targets, not {base:?}"));
        }
        let path = state.join(KEY);
        let key = key(&path, true)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .expect("a key it made");
        let mac = self.mac(&key).expect("a number checked above");
        Ok(format!("<!-- {NAME}: {verdict} {head} on {base} {mac} -->"))
    }

    /// Whether `mac` seals this verdict under the key in `state`: never
    /// with no key there (it makes none), or one that isn't a key.
    pub(crate) fn sealed(&self, state: &Path, mac: &str) -> Result<bool, String> {
        let path = state.join(KEY);
        let Some(key) = key(&path, false).map_err(|e| format!("{}: {e}", path.display()))? else {
            return Ok(false);
        };
        Ok(self
            .mac(&key)
            .is_some_and(|want| bool::from(want.as_bytes().ct_eq(mac.as_bytes()))))
    }

    /// Its MAC under `key`, the repo in lower case: the sweep reads it from
    /// a clone's remote, the steward as GitHub spells it, and GitHub doesn't
    /// mind case. None for a number a marker can't name (`01`, `+1`).
    fn mac(&self, key: &SealKey) -> Option<String> {
        if !NUMBER.is_match(self.n) {
            return None;
        }
        let n = self.n.parse().ok()?;
        Some(key.seal(
            &self.repo.to_lowercase(),
            n,
            NAME,
            self.verdict,
            self.head,
            self.base,
        ))
    }
}

/// The key at `path`; with `create`, a new one if it's missing or isn't one
/// (else None). An empty or short key (a crash or a full disk as it was
/// made) is one anyone could seal with, so a new one, whole or not at all:
/// a missing one is linked in only if no one made one meanwhile, a damaged
/// one replaced. Verdicts sealed with a damaged one stop counting.
fn key(path: &Path, create: bool) -> io::Result<Option<SealKey>> {
    let had = match fs::read(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if let Some(key) = had.as_deref().and_then(parse) {
        return Ok(Some(key));
    }
    if !create {
        return Ok(None);
    }
    if had.is_some() {
        say(&format!("{} isn't a key; making a new one", path.display()));
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.new", std::process::id()));
    let tmp = PathBuf::from(tmp);
    let mut bytes = [0u8; 32];
    let mut filled = 0;
    while filled < bytes.len() {
        filled +=
            rustix::rand::getrandom(&mut bytes[filled..], rustix::rand::GetRandomFlags::empty())?;
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(hex.as_bytes())?;
    f.sync_all()?;
    drop(f);
    if had.is_none() {
        let linked = fs::hard_link(&tmp, path);
        fs::remove_file(&tmp)?;
        match linked {
            Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
            _ => {}
        }
    } else {
        fs::rename(&tmp, path)?;
    }
    parse(&fs::read(path)?)
        .map(Some)
        .ok_or_else(|| io::Error::other("couldn't be written"))
}

/// A key file's text, as a key: its hex, whitespace around it aside.
fn parse(text: &[u8]) -> Option<SealKey> {
    SealKey::from_hex(std::str::from_utf8(text).ok()?.trim()).ok()
}

#[cfg(test)]
mod tests;
