//! A repo on its "GitHub" (a bare repo with main and PR 5's head, as
//! refs/pull/5/head), the machine's checkout of it, and the steward's
//! folders for runs, all in a temporary directory, with real git.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use burrow_proof::Record;
use burrow_proof::fence::Fence;
use tempfile::TempDir;

pub struct World {
    pub tmp: TempDir,
    pub origin: PathBuf,
    pub checkout: PathBuf,
    pub builds: PathBuf,
    pub trees: PathBuf,
    /// PR 5's head.
    pub head: String,
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(["-c", "init.defaultBranch=main", "-C"])
        .arg(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

impl World {
    pub fn new() -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let (origin, work, checkout) = (
            root.join("origin.git"),
            root.join("work"),
            root.join("src/r"),
        );
        let (builds, trees) = (root.join("state/builds"), root.join("proofs"));
        for d in [&origin, &work, &builds, &trees, &root.join("src")] {
            fs::create_dir_all(d).unwrap();
        }
        git(&origin, &["init", "-q", "--bare"]);
        git(&work, &["init", "-q"]);
        let commit = |text: &str, to: &str| {
            fs::write(work.join("f"), text).unwrap();
            git(&work, &["add", "f"]);
            git(&work, &["commit", "-qm", text.trim()]);
            git(
                &work,
                &[
                    "push",
                    "-q",
                    origin.to_str().unwrap(),
                    &format!("HEAD:{to}"),
                ],
            );
            git(&work, &["rev-parse", "HEAD"]).trim().to_owned()
        };
        commit("main\n", "refs/heads/main");
        git(
            &root.join("src"),
            &["clone", "-q", origin.to_str().unwrap(), "r"],
        );
        // Then main moves on, and PR 5 is opened on it.
        commit("main moved on\n", "refs/heads/main");
        let head = commit("pr\n", "refs/pull/5/head");
        World {
            tmp,
            origin,
            checkout,
            builds,
            trees,
            head,
        }
    }

    pub fn path(&self) -> &Path {
        self.tmp.path()
    }

    /// A run of RUNS on PR 5's head, as the steward writes it.
    pub fn record(&self, runs: &[(&str, &str)]) -> Record {
        Record {
            repo: "sam/r".into(),
            number: 5,
            head: self.head.clone(),
            checkout: self.checkout.clone(),
            runs: runs
                .iter()
                .map(|(c, r)| (c.to_string(), r.to_string()))
                .collect::<BTreeMap<_, _>>(),
            unit: "autoland-proof-sam-r-5-1.service".into(),
            timeout: 60,
            bash: which("bash"),
            git: which("git"),
            trees: self.trees.clone(),
            kick: None,
            comment_bytes: 60000,
        }
    }

    /// REC's folder, ID, with its build.json.
    pub fn made(&self, id: &str, rec: &Record) -> PathBuf {
        let dir = self.builds.join(id);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("build.json"), serde_json::to_string(rec).unwrap()).unwrap();
        dir
    }

    /// The checkout's worktrees, itself first.
    pub fn worktrees(&self) -> Vec<String> {
        git(&self.checkout, &["worktree", "list", "--porcelain"])
            .lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .map(str::to_owned)
            .collect()
    }

    pub fn trees_left(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.trees)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

pub fn which(tool: &str) -> PathBuf {
    let out = Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .unwrap();
    let path = String::from_utf8(out.stdout).unwrap().trim().to_owned();
    assert!(!path.is_empty(), "no {tool} on PATH");
    PathBuf::from(path)
}

/// No fence: the command runs as it is (the cgroup tests have the real
/// one). What it was asked to clear, and the cgroup it says it keeps.
#[derive(Default)]
pub struct Plain {
    pub cleared: Vec<usize>,
    pub at: Option<PathBuf>,
}

impl Fence for Plain {
    fn around(&mut self, _: usize, program: &Path, args: &[&str]) -> io::Result<Command> {
        let mut c = Command::new(program);
        c.args(args);
        Ok(c)
    }

    fn clear(&mut self, i: usize) -> io::Result<()> {
        self.cleared.push(i);
        Ok(())
    }

    fn record(&self) -> Option<PathBuf> {
        self.at.clone()
    }
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}
