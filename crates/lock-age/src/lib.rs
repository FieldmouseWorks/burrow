//! `lock-age POLICY RELEASE-AGES BRANCH`: a lock file refresh takes only
//! releases at least the longest release age its Renovate config sets old,
//! or fails.
//!
//! Renovate runs it after a lock file refresh (a postUpgradeTask, from the
//! repo's root, with the refresh in the working tree and HEAD at the base).
//! For each lock file the refresh changed:
//!
//! - Cargo.lock: every crates.io version that isn't in the base's lock file
//!   and was published within the age is taken back with `cargo update
//!   --precise` to the newest version old enough, or to the base's. One that
//!   can't go back because a crate the refresh moved needs it takes that
//!   crate back to its base version too.
//! - package-lock.json: made again from nothing, as the refresh did, with
//!   npm's `--before` set to the cutoff, and failing if npm can't resolve
//!   under it (Renovate drops `--before` and resolves again without it).
//!
//! Then every new version is checked against its registry's publish time. A
//! version the base already had counts as checked only from the same source.
//! One still too young, a registry it can't ask (a crate from another
//! registry, a tarball from elsewhere), a lock file taken all the way back to
//! the base's (Renovate would then commit its own, unchecked), an npm lock
//! file without a packages map (npm 6's) or any other lock file changing
//! (yarn, pnpm) fails the refresh: Renovate shows it as an artifact failure
//! and doesn't merge it. When the only failure is that nothing in a refresh
//! is old enough yet, its last line says when the first of it will be
//! ([`waiting`]), for the pipeline to wait for rather than take it for red
//! CI to fix (autoland/lock_wait.py). A git dependency's commit is taken as
//! it is, as Nix inputs are, in Cargo.lock and package-lock.json alike
//! (burrow#82).
//!
//! POLICY is the machine's own Renovate config (JSON). The age is the
//! longest minimumReleaseAge set anywhere in it, in its globalExtends, or in
//! the repo's own config (in the config files the resolved policy adds, then
//! Renovate's own), with every config migrated and every preset each extends
//! resolved, as RELEASE-AGES (modules/updates/release-ages.mjs, Renovate's
//! own code) reads them; no age set anywhere is none. A preset it can't
//! resolve, an age it can't read, or inheritConfig (config from another repo
//! it can't see) fails the refresh, as does a refresh for a BRANCH other
//! than the default (the remote's HEAD, asked of it now, not the clone's
//! possibly stale origin/HEAD): Renovate reads the default branch's config
//! for it, which isn't the one checked out. In a fork (forkToken) the remote
//! is the fork, not the repo whose default Renovate reads, so a machine
//! config that works in forks fails every refresh. For npm, a cutoff npm's
//! own config sets for the lock file's directory (before, min-release-age)
//! holds too when it's earlier, and a release published after its before is
//! never waited for.

mod cargo;
mod config;
mod git;
mod npm;
pub mod registry;

use std::collections::BTreeMap;
use std::path::Path;

use jiff::{SignedDuration, Timestamp};
use serde_json::Value;

/// crates.io, as a Cargo.lock names it.
pub const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";
/// npm's registry, which a package-lock.json's `resolved` starts with.
pub const NPM: &str = "https://registry.npmjs.org/";

/// lock-age's last line when all it refused is too young, saying when the
/// refresh will have something old enough: autoland/lock_wait.py reads it in
/// Renovate's comment on the failure.
pub fn waiting(at: Timestamp) -> String {
    format!(
        "lock-age: nothing in the refresh is old enough until {}",
        at.strftime("%Y-%m-%dT%H:%M:%SZ")
    )
}

/// One version in crates.io's index.
#[derive(Debug, Clone)]
pub struct Release {
    pub vers: String,
    /// When it was published; the index has no time for the oldest.
    pub pubtime: Option<String>,
    pub yanked: bool,
}

/// What a command printed, and how it ended.
#[derive(Debug, Clone, Default)]
pub struct Out {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// What release-ages says decides a config's age.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ages {
    /// Every minimumReleaseAge it sets, in milliseconds.
    pub ages: Vec<f64>,
    /// Its configFileNames, only when it sets them (null included).
    pub config_file_names: Option<Value>,
    pub inherit_config: bool,
    pub fork_mode: bool,
}

/// What lock-age asks of the world beyond the checkout and its git.
pub trait World {
    /// The crate's versions in crates.io's sparse index.
    fn crate_index(&mut self, name: &str) -> Result<Vec<Release>, String>;
    /// When crates.io's API says a version was published, for one older
    /// than the index's publish times.
    fn crate_created(&mut self, name: &str, version: &str) -> Result<Timestamp, String>;
    /// npm's registry: when each of a package's versions was published.
    fn npm_times(&mut self, name: &str) -> Result<BTreeMap<String, String>, String>;
    /// cargo or npm, run in `dir`.
    fn run(&mut self, argv: &[&str], dir: &Path) -> Out;
    /// release-ages' reading of a Renovate config.
    fn release_ages(&mut self, config: &Value) -> Result<Ages, String>;
    fn now(&self) -> Timestamp;
}

/// What a run says, a line at a time, for its stderr.
pub type Log<'a> = &'a mut dyn FnMut(String);

/// A run in the checkout at `dir`: 0 when the refresh holds, 1 when it
/// doesn't (its reasons logged, and [`waiting`] last when they're all a
/// wait).
pub fn lock_age(dir: &Path, policy: &Path, branch: &str, w: &mut dyn World, log: Log) -> i32 {
    let (age, paths) = match settle(dir, policy, branch, w) {
        Ok(x) => x,
        Err(e) => {
            log(format!("lock-age: {e}"));
            return 1;
        }
    };
    let cutoff = w.now() - age;
    let mut bad = Vec::new();
    // When each lock file that went back to the base's will have something.
    let mut ripe = Vec::new();
    for path in paths {
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let lock = Lock { dir, path: &path };
        let got = match name.as_str() {
            "Cargo.lock" => cargo::refresh(&lock, cutoff, age, w, &mut bad, log),
            "package-lock.json" => npm::refresh(&lock, age, w, &mut bad, log),
            "yarn.lock" | "pnpm-lock.yaml" | "bun.lock" | "bun.lockb" | "npm-shrinkwrap.json" => {
                bad.push(format!(
                    "{path}: lock-age can't check this kind of lock file"
                ));
                Ok(None)
            }
            _ => Ok(None),
        };
        match got {
            Ok(r) => ripe.extend(r),
            // A registry that didn't answer proves nothing.
            Err(e) => bad.push(format!("{path}: {e}")),
        }
    }
    for line in &bad {
        log(format!("lock-age: {line}"));
    }
    if let Some(last) = ripe.iter().max()
        && bad.is_empty()
    {
        // Last, and only when it's all there is: every lock file that went
        // back has something by then.
        log(waiting(*last));
    }
    i32::from(!bad.is_empty() || !ripe.is_empty())
}

/// The age, and the files the refresh changed; or why the refresh can't be
/// checked at all.
fn settle(
    dir: &Path,
    policy: &Path,
    branch: &str,
    w: &mut dyn World,
) -> Result<(SignedDuration, Vec<String>), String> {
    if branch != git::default_branch(dir)? {
        return Err(format!(
            "the refresh is for {branch}, whose config Renovate takes from the default branch, \
             which isn't the one checked out here"
        ));
    }
    let policy = std::fs::read_to_string(policy)
        .map_err(|e| format!("{}: {e}", policy.display()))
        .and_then(|t| serde_json::from_str(&t).map_err(|e| format!("the policy: {e}")))?;
    let age = config::age(dir, &policy, w)?;
    Ok((age, git::changed(dir)?))
}

/// A lock file the refresh changed: its path from the checkout's root.
struct Lock<'a> {
    dir: &'a Path,
    path: &'a str,
}

impl Lock<'_> {
    /// Where it is.
    fn file(&self) -> std::path::PathBuf {
        self.dir.join(self.path)
    }

    /// The directory it's in, which cargo and npm run in.
    fn home(&self) -> std::path::PathBuf {
        self.file()
            .parent()
            .map_or_else(|| self.dir.to_path_buf(), Path::to_path_buf)
    }

    fn read(&self) -> Result<String, String> {
        std::fs::read_to_string(self.file()).map_err(|e| format!("can't read it: {e}"))
    }

    /// The base's copy, if it had one.
    fn base(&self) -> Option<String> {
        git::base_text(self.dir, self.path)
    }

    /// Whether it's the base's again.
    fn same_as_base(&self, base: Option<&str>) -> bool {
        base.is_some_and(|b| self.read().is_ok_and(|t| t == b))
    }
}

/// Sorts versions oldest first; a pre-release sorts before its release.
fn semver_key(v: &str) -> (Vec<u64>, u8, String) {
    let v = v.split('+').next().unwrap_or(v);
    let (core, pre) = v.split_once('-').unwrap_or((v, ""));
    let nums = core
        .split('.')
        .map(|x| match x.bytes().all(|b| b.is_ascii_digit()) {
            true => x.parse().unwrap_or(u64::MAX),
            false => 0,
        })
        .collect();
    (nums, u8::from(pre.is_empty()), pre.to_owned())
}

/// A registry's publish time.
fn when(s: &str) -> Result<Timestamp, String> {
    s.parse()
        .map_err(|e| format!("can't read the time {s:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::{semver_key, waiting};

    /// The steward reads the wait in Renovate's comment on the failure.
    #[test]
    fn the_steward_reads_its_wait() {
        let reader = include_str!("../../../modules/autoland/lock_wait.py");
        let line = waiting(jiff::Timestamp::from_second(1_793_000_000).unwrap());
        let (said, at) = line.split_at(line.len() - 20);
        assert_eq!(at, "2026-10-26T07:33:20Z");
        let pattern =
            format!(r#"WAITING = re.compile(r"{said}(\d{{4}}-\d\d-\d\dT\d\d:\d\d:\d\dZ)")"#);
        assert!(reader.contains(&pattern), "{pattern}");
    }

    #[test]
    fn versions_sort_oldest_first() {
        let mut v = [
            "1.10.0",
            "1.2.0",
            "1.2.0-rc.1",
            "0.9.9",
            "1.2.0+build",
            "2.0.0-alpha",
        ];
        v.sort_by_key(|x| semver_key(x));
        assert_eq!(
            v,
            [
                "0.9.9",
                "1.2.0-rc.1",
                "1.2.0",
                "1.2.0+build",
                "1.10.0",
                "2.0.0-alpha"
            ]
        );
    }
}
