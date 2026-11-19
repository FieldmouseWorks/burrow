//! The machine's build store (`burrow.builds`, docs/gate-design.md, "One
//! build store, written by whoever may sign"): a Nix binary cache in a
//! directory nginx serves as it is, and the door that writes it.
//!
//! The door takes one upload a request: a manifest of store paths, their
//! NARs, and the job's GitHub OIDC token, whose audience names the
//! manifest's digest ([`audience`]). It admits the job by Conary's rule,
//! asked of `cn attest job` ([`door`]), writes each NAR only once its hash is
//! the manifest's, signs its narinfo with the machine's key, and drops what
//! was fetched longest ago past the store's cap ([`Store::prune`]).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use ed25519_dalek::{Signer as _, SigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod door;

/// An upload's token's audience: this, then its manifest's SHA-256 in
/// lowercase hex, as an attestation's names its statement.
pub const AUDIENCE: &str = "burrow-builds:v1:sha256:";

/// The store directory every path it holds is in.
pub const STORE_DIR: &str = "/nix/store";

/// The audience a job's token names for `manifest`, its bytes as sent.
pub fn audience(manifest: &[u8]) -> String {
    format!("{AUDIENCE}{}", hex(&Sha256::digest(manifest)))
}

/// What an upload says it holds: each path, as `nix path-info --json` says
/// it, in the order its NARs follow.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub paths: Vec<Entry>,
}

/// One store path an upload holds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Entry {
    /// Its full path, under [`STORE_DIR`].
    pub path: String,
    /// `sha256:` and the NAR's hash, in Nix's base-32.
    pub nar_hash: String,
    pub nar_size: u64,
    /// The full paths it refers to, itself included where it does.
    #[serde(default)]
    pub references: Vec<String>,
    #[serde(default)]
    pub deriver: Option<String>,
}

impl Manifest {
    /// A manifest from its bytes, every path a store path, each once, and
    /// every hash Nix's form of a SHA-256.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let manifest: Self =
            serde_json::from_slice(bytes).map_err(|e| format!("the manifest doesn't read: {e}"))?;
        let mut seen = BTreeSet::new();
        for e in &manifest.paths {
            hash_part(&e.path)?;
            if !seen.insert(&e.path) {
                return Err(format!("{} is in the manifest twice", e.path));
            }
            for r in e.references.iter().chain(&e.deriver) {
                hash_part(r)?;
            }
            nar_digest(&e.nar_hash)?;
        }
        Ok(manifest)
    }
}

/// A store path's hash part, the name its narinfo is kept by, once it's a
/// store path: `/nix/store/`, 32 base-32 characters, a dash and a name.
pub fn hash_part(path: &str) -> Result<&str, String> {
    let bad = || format!("{path:?} isn't a store path");
    let base = path
        .strip_prefix(STORE_DIR)
        .and_then(|p| p.strip_prefix('/'))
        .ok_or_else(bad)?;
    let (hash, name) = base.split_at_checked(32).ok_or_else(bad)?;
    let name = name.strip_prefix('-').ok_or_else(bad)?;
    let named = |c: char| c.is_ascii_alphanumeric() || "+-._?=".contains(c);
    if !hash.bytes().all(|b| ALPHABET.contains(&b))
        || name.is_empty()
        || name.starts_with('.')
        || !name.chars().all(named)
    {
        return Err(bad());
    }
    Ok(hash)
}

/// A `sha256:` hash in Nix's base-32, as its 32 bytes' encoding.
fn nar_digest(hash: &str) -> Result<&str, String> {
    let digest = hash
        .strip_prefix("sha256:")
        .filter(|d| d.len() == 52 && d.bytes().all(|b| ALPHABET.contains(&b)))
        .ok_or_else(|| format!("{hash:?} isn't a sha256 in Nix's base-32"))?;
    Ok(digest)
}

/// Nix's base-32 alphabet: no e, o, u or t.
const ALPHABET: &[u8; 32] = b"0123456789abcdfghijklmnpqrsvwxyz";

/// `bytes` in Nix's base-32, as Nix writes a hash.
pub fn nix32(bytes: &[u8]) -> String {
    let len = (bytes.len() * 8).div_ceil(5);
    (0..len)
        .rev()
        .map(|n| {
            let (b, i) = (n * 5 / 8, n * 5 % 8);
            let low = bytes[b] >> i;
            let high = bytes
                .get(b + 1)
                .map_or(0, |h| h.checked_shl(8 - i as u32).unwrap_or(0));
            ALPHABET[usize::from((low | high) & 0x1f)] as char
        })
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The machine's signing key, in Nix's form (`NAME:BASE64`), made on first
/// start and never handed to a run.
pub struct Key {
    name: String,
    key: SigningKey,
}

impl Key {
    /// The key at `path`, made there first when there's none.
    pub fn open_or_make(path: &Path, name: &str) -> Result<Self, String> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(text.trim()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let mut seed = [0; 32];
                getrandom::fill(&mut seed).map_err(|e| format!("no randomness: {e}"))?;
                let key = Self {
                    name: name.to_owned(),
                    key: SigningKey::from_bytes(&seed),
                };
                let mut bytes = seed.to_vec();
                bytes.extend(key.key.verifying_key().as_bytes());
                write_new(
                    path,
                    format!("{name}:{}", STANDARD.encode(bytes)).as_bytes(),
                    0o600,
                )
                .map_err(|e| format!("{} doesn't write: {e}", path.display()))?;
                Ok(key)
            }
            Err(e) => Err(format!("{} doesn't read: {e}", path.display())),
        }
    }

    /// The key at `path`.
    pub fn open(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path)
            .map_err(|e| format!("{} doesn't read: {e}", path.display()))?;
        Self::parse(text.trim())
    }

    /// A secret key as `nix-store --generate-binary-cache-key` writes one.
    pub fn parse(text: &str) -> Result<Self, String> {
        let bad = || "the store's key isn't NAME:BASE64 of 64 bytes".to_owned();
        let (name, b64) = text.split_once(':').ok_or_else(bad)?;
        let bytes = STANDARD.decode(b64).map_err(|_| bad())?;
        let seed: [u8; 32] = bytes
            .get(..32)
            .and_then(|s| s.try_into().ok())
            .ok_or_else(bad)?;
        let key = SigningKey::from_bytes(&seed);
        if bytes.len() != 64 || bytes[32..] != key.verifying_key().as_bytes()[..] {
            return Err(bad());
        }
        Ok(Self {
            name: name.to_owned(),
            key,
        })
    }

    /// Its public half, as a store that trusts it names it.
    pub fn public(&self) -> String {
        format!(
            "{}:{}",
            self.name,
            STANDARD.encode(self.key.verifying_key().as_bytes())
        )
    }

    /// Its signature of a path's fingerprint, as a narinfo's `Sig`.
    fn sign(&self, e: &Entry) -> String {
        // As Nix prints a path set: sorted, each once, whatever order the
        // manifest lists them in.
        let refs: BTreeSet<&str> = e.references.iter().map(String::as_str).collect();
        let refs: Vec<&str> = refs.into_iter().collect();
        let fingerprint = format!(
            "1;{};{};{};{}",
            e.path,
            e.nar_hash,
            e.nar_size,
            refs.join(",")
        );
        let sig = self.key.sign(fingerprint.as_bytes());
        format!("{}:{}", self.name, STANDARD.encode(sig.to_bytes()))
    }
}

/// A file written whole or not at all: beside its place, then renamed in.
fn write_new(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = path.parent().unwrap_or(Path::new("."));
    let mut f = tempfile::NamedTempFile::new_in(dir)?;
    f.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    f.write_all(bytes)?;
    f.as_file().sync_all()?;
    f.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// The cache: `nix-cache-info`, a narinfo per path, its NAR under `nar/`
/// by the path's hash part, and under `fetched/` a file per NAR that nginx appends a line to each
/// time it serves it, so its time says when it was last fetched.
pub struct Store {
    pub dir: PathBuf,
    /// The most bytes its NARs and narinfos may take.
    pub max_size: u64,
}

/// What an upload did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Uploaded {
    pub written: Vec<String>,
    pub skipped: Vec<String>,
    pub dropped: Vec<String>,
}

/// Why an upload stopped, and what it wrote before it did: each path
/// written was checked whole first.
#[derive(Debug)]
pub enum Stopped {
    /// A NAR's bytes weren't the manifest's.
    Mismatch {
        path: String,
        done: Uploaded,
    },
    /// The body ended early, or had more than the manifest said.
    Short {
        done: Uploaded,
    },
    Io(io::Error),
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mismatch { path, .. } => write!(f, "{path}'s NAR isn't the manifest's"),
            Self::Short { .. } => f.write_str("the body isn't the NARs the manifest lists"),
            Self::Io(e) => write!(f, "the store doesn't write: {e}"),
        }
    }
}

impl From<io::Error> for Stopped {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl Store {
    /// Makes the store's directories and its `nix-cache-info`, and puts the
    /// key's public half beside them.
    pub fn init(&self, key: &Key) -> io::Result<()> {
        for d in ["nar", "fetched", "tmp"] {
            fs::create_dir_all(self.dir.join(d))?;
        }
        write_new(
            &self.dir.join("nix-cache-info"),
            format!("StoreDir: {STORE_DIR}\nWantMassQuery: 1\nPriority: 40\n").as_bytes(),
            0o644,
        )?;
        write_new(
            &self.dir.join("public-key"),
            format!("{}\n", key.public()).as_bytes(),
            0o644,
        )
    }

    fn narinfo(&self, path: &str) -> PathBuf {
        let hash = hash_part(path).unwrap_or_default();
        self.dir.join(format!("{hash}.narinfo"))
    }

    /// Whether the store holds `path`.
    pub fn has(&self, path: &str) -> bool {
        self.narinfo(path).exists()
    }

    /// Writes what `body` holds, the NARs of `manifest`'s paths in its
    /// order, under the store's lock: each checked against the manifest
    /// before its narinfo is written and signed, one the store holds already
    /// read and let go. Then prunes past the cap.
    pub fn upload(
        &self,
        manifest: &Manifest,
        body: &mut dyn Read,
        key: &Key,
    ) -> Result<Uploaded, Stopped> {
        let lock = File::create(self.dir.join("tmp/lock"))?;
        lock.lock()?;
        let mut done = Uploaded::default();
        for e in &manifest.paths {
            let mut tmp = tempfile::NamedTempFile::new_in(self.dir.join("tmp"))?;
            let mut hasher = Sha256::new();
            let mut nar = (&mut *body).take(e.nar_size);
            let mut buf = vec![0; 1 << 16];
            let mut got = 0;
            loop {
                let n = nar.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
                tmp.write_all(&buf[..n])?;
                got += n as u64;
            }
            if got != e.nar_size {
                return Err(Stopped::Short { done });
            }
            let digest = nix32(&hasher.finalize());
            if nar_digest(&e.nar_hash).ok() != Some(digest.as_str()) {
                return Err(Stopped::Mismatch {
                    path: e.path.clone(),
                    done,
                });
            }
            if self.has(&e.path) {
                done.skipped.push(e.path.clone());
                continue;
            }
            // Public, as the cache is: nginx serves it.
            tmp.as_file()
                .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o644))?;
            tmp.as_file().sync_all()?;
            // Named by its path, not its hash: two paths can have one NAR,
            // and each goes on its own.
            let nar = format!("nar/{}.nar", hash_part(&e.path).unwrap_or_default());
            tmp.persist(self.dir.join(&nar)).map_err(|e| e.error)?;
            // A narinfo last, so no reader finds one without its NAR.
            write_new(
                &self.narinfo(&e.path),
                self.info(e, &nar, key).as_bytes(),
                0o644,
            )?;
            done.written.push(e.path.clone());
        }
        if body.read(&mut [0])? != 0 {
            return Err(Stopped::Short { done });
        }
        done.dropped = self.prune()?;
        Ok(done)
    }

    /// A path's narinfo, its NAR at `url` as it is, signed.
    fn info(&self, e: &Entry, url: &str, key: &Key) -> String {
        let base = |p: &str| p.rsplit('/').next().unwrap_or_default().to_owned();
        let refs: Vec<String> = e.references.iter().map(|r| base(r)).collect();
        let mut info = format!(
            "StorePath: {}\nURL: {url}\nCompression: none\nFileHash: {}\nFileSize: {}\nNarHash: {}\nNarSize: {}\nReferences: {}\n",
            e.path,
            e.nar_hash,
            e.nar_size,
            e.nar_hash,
            e.nar_size,
            refs.join(" ")
        );
        if let Some(d) = &e.deriver {
            info.push_str(&format!("Deriver: {}\n", base(d)));
        }
        info.push_str(&format!("Sig: {}\n", key.sign(e)));
        info
    }

    /// Drops what was fetched longest ago (its NAR's `fetched/` file's time,
    /// or when it was written for one never fetched) until what the store
    /// holds fits its cap: each narinfo first, so no reader finds one
    /// without its NAR. What it dropped, by path. Called under the lock.
    pub fn prune(&self) -> io::Result<Vec<String>> {
        let mut held = Vec::new();
        let mut total = 0;
        for f in fs::read_dir(&self.dir)? {
            let f = f?;
            let name = f.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".narinfo") {
                continue;
            }
            let text = fs::read_to_string(f.path())?;
            let field = |k: &str| {
                text.lines()
                    .find_map(|l| l.strip_prefix(k).map(str::to_owned))
                    .unwrap_or_default()
            };
            let (path, url) = (field("StorePath: "), field("URL: "));
            let nar = self.dir.join(&url);
            let size = f.metadata()?.len() + fs::metadata(&nar).map_or(0, |m| m.len());
            let fetched = self
                .dir
                .join("fetched")
                .join(nar.file_name().unwrap_or_default());
            let at = fs::metadata(&fetched)
                .or_else(|_| fs::metadata(&nar))
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            total += size;
            held.push((at, f.path(), nar, fetched, path, size));
        }
        held.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let mut dropped = Vec::new();
        for (_, info, nar, fetched, path, size) in held {
            if total <= self.max_size {
                break;
            }
            fs::remove_file(&info)?;
            for gone in [nar, fetched] {
                match fs::remove_file(gone) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
            total -= size;
            dropped.push(path);
        }
        Ok(dropped)
    }
}

#[cfg(test)]
pub(crate) mod tests;
