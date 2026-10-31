//! What every workflow test does: read a workflow, and run one of its steps as
//! a runner runs it, in a scratch directory of its own with stand-ins first
//! on its PATH.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::RwLock;

use tempfile::TempDir;
use yaml_serde::Value;

/// A file of the repo, by its path from the repo's root.
pub fn repo_file(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

pub fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub fn yaml(text: &str) -> Value {
    yaml_serde::from_str(text).unwrap_or_else(|e| panic!("{e}: {text}"))
}

/// A workflow (or any YAML file) of the repo, its anchors resolved.
pub fn workflow(path: &str) -> Value {
    yaml(&read(&repo_file(path)))
}

/// The step of a job with this id.
pub fn step<'a>(job: &'a Value, id: &str) -> &'a Value {
    steps(job)
        .iter()
        .find(|s| s["id"] == id)
        .unwrap_or_else(|| panic!("no step {id}"))
}

pub fn steps(job: &Value) -> &Vec<Value> {
    job["steps"].as_sequence().expect("a job with steps")
}

/// A step's script.
pub fn script(step: &Value) -> &str {
    assert_eq!(step["shell"], "bash", "{step:?}");
    step["run"].as_str().expect("a step that runs a script")
}

/// A string value, or none.
pub fn text(v: &Value) -> &str {
    v.as_str().unwrap_or_else(|| panic!("not a string: {v:?}"))
}

/// Writing an executable while another thread starts a process lets the
/// child inherit the file open for writing until it execs, and running the
/// file then fails with "Text file busy". Writers take this whole, and
/// starting a process takes it shared.
static EXEC: RwLock<()> = RwLock::new(());

/// A process's output, started under [`EXEC`].
pub fn output(mut cmd: Command) -> Output {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = {
        let _shared = EXEC.read().unwrap_or_else(|e| e.into_inner());
        cmd.spawn()
    };
    child
        .and_then(|c| c.wait_with_output())
        .unwrap_or_else(|e| panic!("{cmd:?}: {e}"))
}

/// The first `name` on PATH.
pub fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("no {name} on PATH"))
}

/// What a step did.
pub struct Ran {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    /// What it wrote to GITHUB_OUTPUT.
    pub outputs: String,
}

impl Ran {
    /// Everything it printed (the job's log).
    pub fn said(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// A scratch directory: the runner's machine for one test.
pub struct Scratch {
    dir: TempDir,
}

impl Default for Scratch {
    fn default() -> Self {
        let dir = tempfile::tempdir().expect("a scratch directory");
        fs::create_dir(dir.path().join("bin")).unwrap();
        Scratch { dir }
    }
}

impl Scratch {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn join(&self, path: &str) -> PathBuf {
        self.path().join(path)
    }

    /// A new, empty directory in it.
    pub fn mkdtemp(&self) -> PathBuf {
        tempfile::tempdir_in(self.path()).unwrap().keep()
    }

    /// A stand-in on the steps' PATH: a bash script.
    pub fn stub(&self, name: &str, body: &str) {
        let path = self.join("bin").join(name);
        let _whole = EXEC.write().unwrap_or_else(|e| e.into_inner());
        fs::write(&path, format!("#!{}\n{body}", which("bash").display())).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// PATH with the stand-ins first.
    pub fn path_var(&self) -> String {
        let path = std::env::var("PATH").unwrap_or_default();
        format!("{}:{path}", self.join("bin").display())
    }

    /// A command with nothing of this process's environment but PATH: HOME
    /// and TMPDIR are the scratch directory, and git reads no system config.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("PATH", self.path_var())
            .env("HOME", self.path())
            .env("TMPDIR", self.path())
            .env("GIT_CONFIG_NOSYSTEM", "1");
        cmd
    }

    /// A step's script, run in `cwd` as GitHub runs `shell: bash`, with the
    /// step's environment on top of [`Scratch::command`]'s.
    pub fn bash(&self, script: &str, cwd: &Path, env: &[(&str, &str)]) -> Ran {
        let dir = self.mkdtemp();
        let (file, outputs) = (dir.join("step.sh"), dir.join("output"));
        fs::write(&file, script).unwrap();
        fs::write(&outputs, "").unwrap();
        let mut cmd = self.command("bash");
        cmd.args(["--noprofile", "--norc", "-eo", "pipefail"])
            .arg(&file)
            .current_dir(cwd)
            .env("GITHUB_OUTPUT", &outputs)
            .envs(env.iter().copied());
        let out = output(cmd);
        Ran {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            outputs: read(&outputs),
        }
    }

    /// git, as a test sets repos up: it fails the test if git fails, and
    /// gives what git printed, trimmed.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let mut cmd = self.command("git");
        cmd.args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.org",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(cwd);
        let out = output(cmd);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// Writes `text` to `file` in `repo` and commits it: the commit.
    pub fn commit(&self, repo: &Path, file: &str, text: &str) -> String {
        write(&repo.join(file), text);
        self.git(repo, &["add", file]);
        self.git(repo, &["commit", "-qm", file]);
        self.git(repo, &["rev-parse", "HEAD"])
    }
}

/// Writes a file, making its directory.
pub fn write(path: &Path, text: &str) {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).unwrap();
    }
    fs::write(path, text).unwrap();
}
