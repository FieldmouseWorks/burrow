//! Cargo.lock: every too-young crates.io version the refresh brought in is
//! taken back with `cargo update --precise`.

use std::collections::{BTreeMap, HashSet};

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

use crate::{CRATES_IO, Lock, Log, World, semver_key, when};

/// Passes over a Cargo.lock before what's left is a failure.
const ROUNDS: usize = 6;
/// Older versions asked about per crate per pass.
const TRIES: usize = 5;

#[derive(Debug, Clone, Deserialize)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

impl Package {
    fn ident(&self) -> (&str, &str, Option<&str>) {
        (&self.name, &self.version, self.source.as_deref())
    }
}

#[derive(Deserialize)]
struct File {
    #[serde(default)]
    package: Vec<Package>,
}

fn packages(text: Option<&str>) -> Result<Vec<Package>, String> {
    match text {
        None => Ok(vec![]),
        Some(t) => toml::from_str::<File>(t)
            .map(|f| f.package)
            .map_err(|e| format!("can't read it: {e}")),
    }
}

/// The index's publish time; crates.io's API for a version from before the
/// index had them.
fn published(w: &mut dyn World, name: &str, version: &str) -> Result<Timestamp, String> {
    let time = w
        .crate_index(name)?
        .into_iter()
        .find(|r| r.vers == version)
        .and_then(|r| r.pubtime)
        .filter(|t| !t.is_empty());
    match time {
        Some(t) => when(&t),
        None => w.crate_created(name, version),
    }
}

/// `cargo update --precise`, in the lock file's directory.
fn precise(w: &mut dyn World, lock: &Lock, log: Log, name: &str, version: &str, to: &str) -> bool {
    let spec = format!("{name}@{version}");
    let out = w.run(
        &["cargo", "update", "--quiet", "-p", &spec, "--precise", to],
        &lock.home(),
    );
    if out.code != 0 {
        let last = out.stderr.trim().lines().next_back().unwrap_or("failed");
        log(format!(
            "lock-age: {}: {name} {version} -> {to}: {last}",
            lock.path
        ));
    }
    out.code == 0
}

/// The lock file's packages that the base's doesn't have from the same
/// source (a path crate has none, and is the repo's own).
fn new(lock: &Lock, base: Option<&str>) -> Result<Vec<Package>, String> {
    let had = packages(base)?;
    let had: HashSet<_> = had.iter().map(Package::ident).collect();
    let now = packages(Some(&lock.read()?))?;
    Ok(now
        .iter()
        .filter(|p| p.source.is_some() && !had.contains(&p.ident()))
        .cloned()
        .collect())
}

/// The new ones published within the age. One from another registry goes
/// in `bad`, when there is one.
fn young(
    lock: &Lock,
    base: Option<&str>,
    cutoff: Timestamp,
    w: &mut dyn World,
    mut bad: Option<&mut Vec<String>>,
) -> Result<Vec<Package>, String> {
    let mut young = vec![];
    for p in new(lock, base)? {
        let source = p.source.as_deref().unwrap_or_default();
        if source.starts_with("git+") {
            continue; // a commit, as a Nix input is
        }
        if source != CRATES_IO {
            if let Some(bad) = bad.as_deref_mut() {
                bad.push(format!(
                    "{}: {} {} is from {source}, whose publish times aren't known",
                    lock.path, p.name, p.version
                ));
            }
            continue;
        }
        if published(w, &p.name, &p.version)? > cutoff {
            young.push(p);
        }
    }
    Ok(young)
}

/// Takes the refresh's too young crates back. When that leaves the lock
/// file as the base's, when the first of them is old enough.
pub fn refresh(
    lock: &Lock,
    cutoff: Timestamp,
    age: SignedDuration,
    w: &mut dyn World,
    bad: &mut Vec<String>,
    log: Log,
) -> Result<Option<Timestamp>, String> {
    let path = lock.path;
    let base = lock.base();
    let base = base.as_deref();
    let mut ripe = None;
    for p in young(lock, base, cutoff, w, None)? {
        let at = published(w, &p.name, &p.version)? + age;
        ripe = Some(ripe.map_or(at, |r: Timestamp| r.min(at)));
    }
    // The base's crates.io versions, the only ones to go back to.
    let mut base_versions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in packages(base)? {
        if p.source.as_deref() == Some(CRATES_IO) {
            base_versions.entry(p.name).or_default().push(p.version);
        }
    }
    for _ in 0..ROUNDS {
        let young = young(lock, base, cutoff, w, None)?;
        if young.is_empty() {
            break;
        }
        for p in young {
            let (name, version) = (&p.name, &p.version);
            let mut older: Vec<String> = w
                .crate_index(name)?
                .into_iter()
                .filter(|r| !r.yanked && semver_key(&r.vers) < semver_key(version))
                .map(|r| r.vers)
                .collect();
            older.sort_by_key(|v| std::cmp::Reverse(semver_key(v)));
            let mut done = false;
            for v in older.iter().take(TRIES) {
                if published(w, name, v)? <= cutoff && precise(w, lock, log, name, version, v) {
                    log(format!(
                        "lock-age: {path}: {name} {version} is too new, took {v}"
                    ));
                    done = true;
                    break;
                }
            }
            if !done {
                let mut theirs = base_versions.get(name).cloned().unwrap_or_default();
                theirs.sort_by_key(|v| std::cmp::Reverse(semver_key(v)));
                for v in theirs {
                    if precise(w, lock, log, name, version, &v) {
                        log(format!(
                            "lock-age: {path}: {name} {version} is too new, kept the base's {v}"
                        ));
                        done = true;
                        break;
                    }
                }
            }
            if done {
                continue;
            }
            // Something the refresh moved needs this version: take those
            // back to the base's version, and this one goes back next pass.
            for q in new(lock, base)? {
                let needs = q
                    .dependencies
                    .iter()
                    .any(|x| x.split(' ').next() == Some(name));
                let to = base_versions
                    .get(&q.name)
                    .and_then(|v| v.iter().max_by_key(|v| semver_key(v)));
                if let Some(to) = to
                    && needs
                    && q.source.as_deref() == Some(CRATES_IO)
                    && precise(w, lock, log, &q.name, &q.version, to)
                {
                    log(format!(
                        "lock-age: {path}: {} {} needs {name} {version}, kept the base's {to}",
                        q.name, q.version
                    ));
                }
            }
        }
    }
    for p in young(lock, base, cutoff, w, Some(bad))? {
        bad.push(format!(
            "{path}: {} {} was published within the age, and couldn't be taken back",
            p.name, p.version
        ));
    }
    if lock.same_as_base(base) {
        log(format!(
            "lock-age: {path}: nothing in the refresh was old enough, so it went back to the base's, \
             and Renovate would commit its own instead"
        ));
        if ripe.is_none() {
            bad.push(format!(
                "{path}: the refresh went back to the base's without anything too young in it"
            ));
        }
        return Ok(ripe);
    }
    Ok(None)
}
