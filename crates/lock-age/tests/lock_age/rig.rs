//! A checkout with a remote, and a world of stand-ins for cargo, npm, the
//! registries and release-ages that keeps what it was asked.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use jiff::{SignedDuration, Timestamp};
use lock_age::{Ages, NPM, Out, Release, World};
use serde_json::{Value, json};

pub const CRATES: &str = lock_age::CRATES_IO;
/// The age the default policy sets: 3 days.
pub const AGE: SignedDuration = SignedDuration::from_hours(72);

/// A fixed now, to the second, so every time below is exact.
pub fn now() -> Timestamp {
    Timestamp::from_second(1_793_000_000).unwrap()
}

pub fn ago(d: SignedDuration) -> String {
    (now() - d).strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn old() -> String {
    ago(SignedDuration::from_hours(20 * 24))
}
pub fn new() -> String {
    ago(SignedDuration::from_hours(5))
}
/// Past the 3 days, not 14.
pub fn mid() -> String {
    ago(SignedDuration::from_hours(5 * 24))
}

/// When a release published then is old enough.
pub fn ripe(published: &str) -> Timestamp {
    published.parse::<Timestamp>().unwrap() + AGE
}

/// A Cargo.lock: (name, version, source, dependencies); a source of None is
/// a path crate.
pub fn cargo_lock(pkgs: &[(&str, &str, Option<&str>, &[&str])]) -> String {
    let mut out = "version = 4\n".to_owned();
    for (name, version, source, deps) in pkgs {
        out += &format!("\n[[package]]\nname = \"{name}\"\nversion = \"{version}\"\n");
        if let Some(s) = source {
            out += &format!("source = \"{s}\"\n");
        }
        if !deps.is_empty() {
            out += "dependencies = [\n";
            for d in *deps {
                out += &format!(" \"{d}\",\n");
            }
            out += "]\n";
        }
    }
    out
}

/// crates.io crates, by name and version.
pub fn crates(pkgs: &[(&str, &str)]) -> String {
    cargo_lock(
        &pkgs
            .iter()
            .map(|(n, v)| (*n, *v, Some(CRATES), &[][..]))
            .collect::<Vec<_>>(),
    )
}

/// A package-lock.json with a workspace member and these installed:
/// (key, version, resolved); resolved defaults to npm's registry.
pub fn npm_lock(pkgs: &[(&str, &str, Option<&str>)]) -> String {
    let mut packages = json!({
        "": {"name": "app", "version": "1.0.0"},
        "packages/w": {"name": "w", "version": "0.0.1"},
        "node_modules/w": {"resolved": "packages/w", "link": true},
    });
    for (key, version, resolved) in pkgs {
        let resolved = resolved.map_or_else(|| format!("{NPM}x/-/x-{version}.tgz"), String::from);
        packages[*key] = json!({"version": version, "resolved": resolved});
    }
    serde_json::to_string_pretty(&json!({"lockfileVersion": 3, "packages": packages})).unwrap()
}

/// npm's stand-in: what it makes of a package-lock.json, first from
/// nothing and then over that.
#[derive(Default)]
pub struct Resolves {
    pub lock: Option<String>,
    pub code: i32,
    pub second_code: i32,
    pub second: Option<String>,
}

type Refuse = Box<dyn Fn(&str, &str, &str) -> bool>;

#[derive(Default)]
pub struct Fake {
    /// Each crate's versions and when they came out.
    pub index: BTreeMap<String, BTreeMap<String, String>>,
    pub npm_times: BTreeMap<String, BTreeMap<String, String>>,
    /// cargo update --precise refuses while one holds of (the lock file,
    /// the crate, the version asked for).
    pub refuse: Vec<Refuse>,
    /// npm's own config for the lock file's directory, and for one
    /// directory only, over that.
    pub npmrc: BTreeMap<String, String>,
    pub npmrc_in: BTreeMap<String, BTreeMap<String, String>>,
    pub resolves: Resolves,
    /// What cargo and npm were run with, and where in the checkout.
    pub ran: Vec<(Vec<String>, String)>,
    root: PathBuf,
}

impl Fake {
    pub fn crate_(&mut self, name: &str, versions: &[(&str, String)]) {
        let e = self.index.entry(name.into()).or_default();
        for (v, t) in versions {
            e.insert((*v).into(), t.clone());
        }
    }

    pub fn npm_package(&mut self, name: &str, versions: &[(&str, String)]) {
        let e = self.npm_times.entry(name.into()).or_default();
        for (v, t) in versions {
            e.insert((*v).into(), t.clone());
        }
    }

    /// npm install ran twice, the same both times: that command.
    pub fn installs(&self) -> (Vec<String>, String) {
        let got: Vec<_> = self
            .ran
            .iter()
            .filter(|(c, _)| c[..2] == ["npm", "install"])
            .collect();
        assert!(got.len() == 2 && got[0] == got[1], "{got:?}");
        got[0].clone()
    }

    /// Its `--before`.
    pub fn before(&self) -> Timestamp {
        let (cmd, _) = self.installs();
        let flag = cmd
            .iter()
            .find_map(|x| x.strip_prefix("--before="))
            .unwrap();
        flag.parse().unwrap()
    }

    fn npm_install(&mut self, argv: &[String], dir: &Path, cwd: &str) -> Out {
        let path = dir.join("package-lock.json");
        // Each lock file's first, in its own directory.
        let first = self
            .ran
            .iter()
            .filter(|(c, d)| d == cwd && c[..2] == ["npm", "install"])
            .count()
            == 1;
        assert!(
            path.exists() != first,
            "npm must resolve from nothing, then again over that: {argv:?}"
        );
        let r = &self.resolves;
        if (if first { r.code } else { r.second_code }) != 0 {
            return Out {
                code: 1,
                stderr: "npm error code ETARGET\nnpm error notarget No matching version".into(),
                ..Out::default()
            };
        }
        if first {
            std::fs::write(&path, r.lock.as_deref().expect("a lock to make")).unwrap();
        } else if let Some(s) = &r.second {
            std::fs::write(&path, s).unwrap();
        }
        Out::default()
    }
}

impl World for Fake {
    fn crate_index(&mut self, name: &str) -> Result<Vec<Release>, String> {
        let versions = self.index.get(name).ok_or(format!("no crate {name}"))?;
        Ok(versions
            .iter()
            .map(|(v, t)| Release {
                vers: v.clone(),
                pubtime: Some(t.clone()),
                yanked: false,
            })
            .collect())
    }

    fn crate_created(&mut self, name: &str, version: &str) -> Result<Timestamp, String> {
        Err(format!("crates.io's API asked about {name} {version}"))
    }

    fn npm_times(&mut self, name: &str) -> Result<BTreeMap<String, String>, String> {
        self.npm_times
            .get(name)
            .cloned()
            .ok_or(format!("no package {name}"))
    }

    fn run(&mut self, argv: &[&str], dir: &Path) -> Out {
        let argv: Vec<String> = argv.iter().map(|x| (*x).to_owned()).collect();
        let cwd = dir.strip_prefix(&self.root).unwrap().to_string_lossy();
        let cwd = if cwd.is_empty() {
            ".".to_owned()
        } else {
            cwd.into_owned()
        };
        self.ran.push((argv.clone(), cwd.clone()));
        let a: Vec<&str> = argv.iter().map(String::as_str).collect();
        match a[..] {
            ["cargo", "update", ..] => {
                let at = |f: &str| a[a.iter().position(|x| *x == f).unwrap() + 1];
                let (name, version) = at("-p").split_once('@').unwrap();
                let to = at("--precise");
                let path = dir.join("Cargo.lock");
                let text = std::fs::read_to_string(&path).unwrap();
                if self.refuse.iter().any(|r| r(&text, name, to)) {
                    return Out {
                        code: 101,
                        stderr: format!("error: failed to select a version for {name}"),
                        ..Out::default()
                    };
                }
                let text = text.replace(
                    &format!("name = \"{name}\"\nversion = \"{version}\""),
                    &format!("name = \"{name}\"\nversion = \"{to}\""),
                );
                std::fs::write(&path, text).unwrap();
                Out::default()
            }
            ["npm", "install", ..] => self.npm_install(&argv, dir, &cwd),
            ["npm", "config", "get", ref keys @ ..] => {
                let mut rc = self.npmrc.clone();
                rc.extend(self.npmrc_in.get(&cwd).cloned().unwrap_or_default());
                let stdout = keys
                    .iter()
                    .map(|k| format!("{k}={}\n", rc.get(*k).map_or("null", String::as_str)))
                    .collect();
                Out {
                    stdout,
                    ..Out::default()
                }
            }
            _ => panic!("ran {argv:?}"),
        }
    }

    /// release-ages, Renovate's reading of a config, is checked on its own
    /// with Renovate (flake.nix's release-ages); here a stand-in reads days
    /// and weeks, and knows three presets: "slow" sets 2 weeks, "files" adds
    /// ci/slow.json as a config file, "inherits" inherits config. Any other
    /// preset it can't resolve.
    fn release_ages(&mut self, config: &Value) -> Result<Ages, String> {
        let mut got = Ages {
            inherit_config: config["inheritConfig"].as_bool().unwrap_or(false),
            fork_mode: config
                .get("forkToken")
                .is_some_and(|t| t.as_str() != Some("")),
            config_file_names: config.get("configFileNames").cloned(),
            ..Ages::default()
        };
        fn walk(node: &Value, got: &mut Ages) -> Result<(), String> {
            match node {
                Value::Object(m) => {
                    for (k, v) in m {
                        match k.as_str() {
                            "extends" => {
                                for x in v.as_array().unwrap() {
                                    match x.as_str().unwrap() {
                                        "slow" => got.ages.push(14.0 * 86_400_000.0),
                                        "files" => {
                                            got.config_file_names
                                                .get_or_insert(json!(["ci/slow.json"]));
                                        }
                                        "inherits" => got.inherit_config = true,
                                        x => return Err(format!("can't resolve {x}")),
                                    }
                                }
                            }
                            "minimumReleaseAge" => {
                                if let Some(v) = v.as_str() {
                                    let (n, unit) =
                                        v.split_once(' ').ok_or(format!("can't read {v}"))?;
                                    let n: f64 =
                                        n.parse().map_err(|_| format!("can't read {v}"))?;
                                    let unit = match unit {
                                        "days" => 86_400_000.0,
                                        "weeks" => 604_800_000.0,
                                        _ => return Err(format!("can't read {v}")),
                                    };
                                    got.ages.push(n * unit);
                                } else if !v.is_null() {
                                    return Err(format!("can't read {v}"));
                                }
                            }
                            k if k.starts_with("statusCheck") => {}
                            _ => walk(v, got)?,
                        }
                    }
                }
                Value::Array(a) => {
                    for v in a {
                        walk(v, got)?;
                    }
                }
                _ => {}
            }
            Ok(())
        }
        walk(config, &mut got)?;
        Ok(got)
    }

    fn now(&self) -> Timestamp {
        now()
    }
}

pub fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A checkout of a repo with a remote, at its base commit.
pub struct Rig {
    pub dir: tempfile::TempDir,
    pub origin: tempfile::TempDir,
    pub w: Fake,
    pub said: Vec<String>,
}

impl Rig {
    /// A checkout whose base commit has `files`; the remote's default
    /// branch is main.
    pub fn repo(files: &[(&str, &str)]) -> Rig {
        let rig = Rig::bare();
        for (path, text) in files {
            rig.write(path, text);
        }
        let d = rig.dir.path();
        git(d, &["add", "-A"]);
        git(
            d,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "base",
            ],
        );
        git(d, &["push", "-q", "origin", "HEAD:refs/heads/main"]);
        rig
    }

    /// A checkout with no commit yet, and its remote.
    pub fn bare() -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let origin = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        git(origin.path(), &["init", "-q", "--bare"]);
        git(origin.path(), &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(
            dir.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        let mut w = Fake {
            root: dir.path().to_path_buf(),
            ..Fake::default()
        };
        // The crates and packages most cases take.
        w.crate_("b", &[("2.0.0", old()), ("2.1.0", old())]);
        w.crate_("c", &[("0.4.0", old()), ("0.4.1", new())]);
        w.crate_("h", &[("1.0.0", old()), ("1.1.0", mid())]);
        w.npm_package(
            "left-pad",
            &[("1.0.0", old()), ("1.1.0", old()), ("1.2.0", new())],
        );
        w.npm_package("@s/k", &[("2.0.0", old()), ("2.0.1", old())]);
        Rig {
            dir,
            origin,
            w,
            said: vec![],
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn write(&self, path: &str, text: &str) {
        let p = self.path().join(path);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.path().join(path)).unwrap()
    }

    /// lock-age on the checkout, with the machine's `policy`, for `branch`.
    pub fn run(&mut self, policy: &Value, branch: &str) -> i32 {
        self.w.ran.clear();
        self.said.clear();
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), policy.to_string()).unwrap();
        let mut said = vec![];
        let dir = self.path().to_path_buf();
        let code = lock_age::lock_age(&dir, file.path(), branch, &mut self.w, &mut |l| {
            said.push(l)
        });
        self.said = said;
        code
    }

    /// lock-age with the default policy, 3 days, for main.
    pub fn lock_age(&mut self) -> i32 {
        self.run(&policy(), "main")
    }

    /// lock-age's answer, and when its last line says the refresh has
    /// something old enough (None: it doesn't say).
    pub fn waiting(&mut self, policy: &Value) -> (i32, Option<Timestamp>) {
        let code = self.run(policy, "main");
        let prefix = lock_age::waiting(now());
        let prefix = &prefix[..prefix.len() - "2026-10-25T00:00:00Z".len()];
        let at = self
            .said
            .last()
            .and_then(|l| l.strip_prefix(prefix))
            .map(|t| t.parse().unwrap());
        (code, at)
    }

    /// The crates a Cargo.lock holds, by name.
    pub fn versions(&self, path: &str) -> BTreeMap<String, String> {
        let lock: toml::Table = toml::from_str(&self.read(path)).unwrap();
        lock["package"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                (
                    p["name"].as_str().unwrap().into(),
                    p["version"].as_str().unwrap().into(),
                )
            })
            .collect()
    }

    pub fn ran_nothing(&self) -> bool {
        self.w.ran.is_empty()
    }
}

/// The machine's Renovate config most cases have: 3 days.
pub fn policy() -> Value {
    json!({"minimumReleaseAge": "3 days"})
}

/// The default policy, and more.
pub fn policy_with(more: Value) -> Value {
    let mut p = policy();
    p.as_object_mut()
        .unwrap()
        .extend(more.as_object().unwrap().clone());
    p
}

pub fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect()
}
