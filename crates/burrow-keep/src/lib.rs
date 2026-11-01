//! burrow-keep: what a push to the default branch keeps of its gate's
//! builds, for later runs of the same leg to substitute from
//! (flake-check.yml's keep-builds). It runs at the end of a leg whose gate
//! passed, with what the last such push kept restored to `DIR/kept`, a
//! binary cache nix substitutes from unsigned, and leaves what this push
//! keeps there instead, for the Actions cache to save:
//!
//!   - every path in the store no one signed (what this gate built, or took
//!     from the kept builds; a public cache's are signed), but derivations,
//!     which each evaluation makes again;
//!   - and what the kept builds hold that a derivation here still makes,
//!     though this gate didn't need it: a check that hadn't changed came
//!     whole from them, without what it was built from, which the next
//!     change to it needs again. What no derivation here makes is dropped.
//!
//! nix writes the cache, but a binary cache it writes holds the whole
//! closure of what it's given: it adds a path only once it can read each
//! path it refers to there, and writes every one it can't. A public cache
//! serves most of that closure, and it's gigabytes a leg. So each path of
//! the closure keep doesn't want stands in as a narinfo naming no NAR,
//! nix writes only the rest, and the stand-ins go again: what's left refers
//! to paths a public cache serves, which nix fetches from there when it
//! substitutes from this one. What the kept builds already hold is linked
//! across, for nix not to write it again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

pub mod nix;

/// The local store's paths, by full name (`/nix/store/…`).
#[derive(Debug, Default)]
pub struct Store {
    /// Where its paths are: `/nix/store`.
    pub dir: String,
    pub paths: BTreeMap<String, Info>,
}

/// What nix has of a path: whether anyone signed it, the paths it refers
/// to, by full name, and its NAR's hash (`sha256-…`) and size.
#[derive(Debug, Clone, Default)]
pub struct Info {
    pub signed: bool,
    pub references: Vec<String>,
    pub nar_hash: String,
    pub nar_size: u64,
}

/// What the keeping reads from nix and has it do.
pub trait Nix {
    /// The local store, as it is now.
    fn store(&self) -> Result<Store, String>;
    /// The outputs these derivations make.
    fn outputs(&self, drvs: &[String]) -> Result<BTreeSet<String>, String>;
    /// Copy these paths, and every path they refer to, into the binary cache
    /// at `to`, but what it holds already.
    fn copy(&self, paths: &[String], to: &Path) -> Result<(), String>;
}

/// What a push kept: how many paths of its own, how many from the kept
/// builds, and the bytes of the cache they make.
#[derive(Debug, PartialEq, Eq)]
pub struct Kept {
    pub gate: usize,
    pub carried: usize,
    pub bytes: u64,
}

/// A path in a binary cache: its narinfo's file name, and the NAR that
/// names, under `nar/`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    narinfo: String,
    nar: String,
}

/// Keep this gate's builds, in place of the kept builds in `dir/kept`,
/// which move to `dir/restored`: aside rather than deleted, as nix's
/// daemon, as root, may have written into them. Nothing moves until what's
/// kept is ready, so a failure leaves `dir/kept` as it was.
pub fn keep(nix: &dyn Nix, dir: &Path) -> Result<Kept, String> {
    let (kept, keep, restored) = (dir.join("kept"), dir.join("keep"), dir.join("restored"));
    let store = nix.store()?;
    let gate: Vec<String> = store
        .paths
        .iter()
        .filter(|(p, info)| !info.signed && !p.ends_with(".drv"))
        .map(|(p, _)| p.clone())
        .collect();
    let drvs: Vec<String> = store
        .paths
        .keys()
        .filter(|p| p.ends_with(".drv"))
        .cloned()
        .collect();
    let made = nix.outputs(&drvs)?;
    let before = entries(&kept)?;
    let carried: Vec<String> = before
        .keys()
        .filter(|p| made.contains(*p) && !store.paths.contains_key(*p))
        .cloned()
        .collect();

    clear(&keep)?;
    fs::create_dir_all(keep.join("nar")).map_err(|e| io(&keep, e))?;
    write(
        &keep.join("nix-cache-info"),
        &format!("StoreDir: {}\n", store.dir),
    )?;
    let wanted: BTreeSet<&String> = gate.iter().chain(&carried).collect();
    for p in &wanted {
        if let Some(e) = before.get(*p) {
            link(&kept, &keep, e)?;
        }
    }
    let mut stand_ins = Vec::new();
    for p in closure(&store, &gate)? {
        if !wanted.contains(p) {
            stand_ins.push(stand_in(&keep, p, &store.paths[p])?);
        }
    }
    if !gate.is_empty() {
        nix.copy(&gate, &keep)?;
    }
    for f in &stand_ins {
        fs::remove_file(f).map_err(|e| io(f, e))?;
    }
    let bytes = size(&keep)?;

    clear(&restored)?;
    fs::rename(&kept, &restored).map_err(|e| io(&kept, e))?;
    fs::rename(&keep, &kept).map_err(|e| io(&keep, e))?;
    Ok(Kept {
        gate: gate.len(),
        carried: carried.len(),
        bytes,
    })
}

fn io(path: &Path, e: std::io::Error) -> String {
    format!("{}: {e}", path.display())
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| io(path, e))
}

fn clear(dir: &Path) -> Result<(), String> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(io(dir, e)),
        _ => Ok(()),
    }
}

/// A binary cache's paths. A narinfo that doesn't parse, or whose NAR isn't
/// a file of its own under `nar/`, isn't one of them.
fn entries(cache: &Path) -> Result<BTreeMap<String, Entry>, String> {
    let mut out = BTreeMap::new();
    for f in fs::read_dir(cache).map_err(|e| io(cache, e))? {
        let f = f.map_err(|e| io(cache, e))?;
        let name = f.file_name().to_string_lossy().into_owned();
        if !name.ends_with(".narinfo") || !f.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        let text = fs::read_to_string(f.path()).map_err(|e| io(&f.path(), e))?;
        let field = |key: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix(": ")))
                .map(str::to_owned)
        };
        let (Some(path), Some(url)) = (field("StorePath"), field("URL")) else {
            continue;
        };
        let nar = match url.strip_prefix("nar/") {
            Some(n) if !n.is_empty() && !n.contains('/') && n != "." && n != ".." => n.to_owned(),
            _ => continue,
        };
        out.insert(path, Entry { narinfo: name, nar });
    }
    Ok(out)
}

/// These paths and every path they refer to, near or far.
fn closure<'a>(store: &'a Store, paths: &'a [String]) -> Result<BTreeSet<&'a String>, String> {
    let mut seen = BTreeSet::new();
    let mut todo: Vec<&String> = paths.iter().collect();
    while let Some(p) = todo.pop() {
        if !seen.insert(p) {
            continue;
        }
        let info = store
            .paths
            .get(p)
            .ok_or_else(|| format!("{p}: not in the store, though a path there refers to it"))?;
        todo.extend(&info.references);
    }
    Ok(seen)
}

/// A narinfo for this path that names no NAR: nix reads it as the path's,
/// there for what nix writes to refer to, and writes the path no more.
/// `create_new`, as a narinfo already there is a link into the kept builds.
fn stand_in(cache: &Path, path: &str, info: &Info) -> Result<PathBuf, String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let hash = name.split_once('-').map_or(name, |(h, _)| h);
    let file = cache.join(format!("{hash}.narinfo"));
    let text = format!(
        "StorePath: {path}\nURL: nar/{STAND_IN}\nCompression: none\nNarHash: {}\nNarSize: {}\n",
        info.nar_hash, info.nar_size
    );
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| io(&file, e))?;
    Ok(file)
}

/// The NAR a stand-in names, which no cache holds: nix names its own for
/// their hashes.
const STAND_IN: &str = "stand-in";

/// The path's narinfo and NAR from one cache in another, as hard links: the
/// kept builds are on the same disk, and nothing writes to their files.
fn link(from: &Path, to: &Path, e: &Entry) -> Result<(), String> {
    for name in [e.narinfo.clone(), format!("nar/{}", e.nar)] {
        let (src, dst) = (from.join(&name), to.join(&name));
        match fs::hard_link(&src, &dst) {
            Err(err) if err.kind() != ErrorKind::AlreadyExists => return Err(io(&src, err)),
            _ => {}
        }
    }
    Ok(())
}

/// The bytes of every file under `dir`.
fn size(dir: &Path) -> Result<u64, String> {
    let mut total = 0;
    for f in fs::read_dir(dir).map_err(|e| io(dir, e))? {
        let f = f.map_err(|e| io(dir, e))?;
        let meta = f.metadata().map_err(|e| io(&f.path(), e))?;
        total += if meta.is_dir() {
            size(&f.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}

#[cfg(test)]
mod tests;
