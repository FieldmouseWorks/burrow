//! A machine for the steps' tests: its clone of the flake at main, GitHub's
//! copy beside it as a bare repo, HOST-update's state, and stand-ins for
//! what the steps run as the machine's user.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use crate::{Config, Train};

/// The branch its PR comes from, in a train repo.
pub const BRANCH: &str = "box-update";

/// runuser and nix, proj and heavy as it runs them, written once for every
/// test: a file being written while another test starts a process is open
/// for writing in that process until it execs, and running the file then
/// fails with "Text file busy". Each keeps what a test needs to see under
/// the HOME it runs with, the test's own.
///
/// - runuser runs what it's given as it is, as the machine's user would,
///   with these first on PATH; before a push from the clone WORK it runs
///   the test's hook, WORK.before-push, when there is one (a hook that's to
///   run once removes itself).
/// - nix logs each call to HOME/nix.log, and fails as a failed build does
///   with HOME/nix-down there, or says nothing and goes with
///   HOME/nix-says-nothing there. A system it builds is made of flake.lock and
///   system.nix and stamped with the commit it's built from, as a real one
///   is (a README, say, goes into neither): HEAD's as update.sh asks for it
///   (WORK#...toplevel, stamped with HEAD), or a commit's as
///   same_as_running does (an --expr with git+file://WORK?rev=COMMIT,
///   stamped with what it forces). Each tree it builds goes to HOME/built. A
///   VM test (#checks.) says what it ran on stdout and stderr both, and
///   fails with HOME/check-fails there.
/// - heavy takes a slot (HOME/nix.log says so) and runs what follows `--`.
/// - proj logs each call to HOME/proj.log and says its job's id, unless
///   HOME/proj-down is there: then it fails.
pub fn bin() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        for (name, text) in [
            (
                "runuser",
                r#"#!/bin/sh
[ "$1" = -u ] && [ "$2" = sam ] && [ "$3" = -- ] && [ "$4" = env ] || exit 9
shift 3
PATH=${0%/*}:$PATH
export PATH
if [ "$4 $5 $7" = "git -C push" ] && [ -e "$6.before-push" ]; then
  sh "$6.before-push"
fi
exec "$@"
"#,
            ),
            (
                "nix",
                r#"#!/bin/sh
printf '%s\n' "$*" >>"$HOME/nix.log"
[ ! -e "$HOME/nix-down" ] || { echo "error: build failed" >&2; exit 1; }
[ ! -e "$HOME/nix-says-nothing" ] || exit 0
case "$*" in *'#checks.'*)
  echo "ran $*"
  echo "a VM test's log" >&2
  [ ! -e "$HOME/check-fails" ]
  exit
esac
expr= ref=
while [ $# -gt 0 ]; do
  case $1 in
    --expr) expr=$2; shift ;;
    *'#nixosConfigurations.'*) ref=$1 ;;
  esac
  shift
done
if [ -n "$expr" ]; then
  dir=$(printf '%s\n' "$expr" | sed -n 's|.*git+file://\([^?]*\)?rev=.*|\1|p')
  commit=$(printf '%s\n' "$expr" | sed -n 's|.*?rev=\([0-9a-f]*\).*|\1|p')
  stamp=$(printf '%s\n' "$expr" | sed -n 's|.*mkForce \([^;]*\);.*|\1|p')
else
  dir=${ref%%#*}
  commit=$(git -C "$dir" rev-parse HEAD)
  stamp="\"$commit\""
fi
git -C "$dir" rev-parse "$commit^{tree}" >>"$HOME/built"
echo "/nix/store/$(printf '%s\n' "$(git -C "$dir" ls-tree "$commit" -- flake.lock system.nix)" "$stamp" | sha256sum | cut -c1-32)-nixos-system"
"#,
            ),
            (
                "heavy",
                r#"#!/bin/sh
[ "$1" = -- ] || exit 9
shift
echo "slot taken" >>"$HOME/nix.log"
exec "$@"
"#,
            ),
            (
                "proj",
                r#"#!/bin/sh
printf '%s\n' "$*" >>"$HOME/proj.log"
[ ! -e "$HOME/proj-down" ] || { echo "proj: no slot for it" >&2; exit 1; }
echo "homelab-$(wc -l <"$HOME/proj.log" | tr -d ' ')"
"#,
            ),
        ] {
            let path = bin.path().join(name);
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    })
    .path()
}

pub fn train() -> Train {
    Train {
        repo: "sam/homelab".into(),
        branch: BRANCH.into(),
        settled: "review-settled".into(),
        context: "conary/train".into(),
        steward: "sams-steward".into(),
    }
}

/// A machine's clone of its flake at main, GitHub's copy beside it as a
/// bare repo with one commit on main, and HOST-update's state, with nothing
/// in it yet but its logs and jobs dirs: in a train repo, on a machine
/// without heavy slots or an App.
pub struct Machine {
    pub dir: tempfile::TempDir,
    pub cfg: Config,
}

impl Machine {
    pub fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        let home = d.join("home");
        fs::create_dir(&home).unwrap();
        fs::write(
            home.join(".gitconfig"),
            "[user]\n\tname = test\n\temail = test@example.org\n[init]\n\tdefaultBranch = main\n[advice]\n\tdetachedHead = false\n",
        )
        .unwrap();
        fs::create_dir_all(d.join("state/logs")).unwrap();
        fs::create_dir(d.join("state/jobs")).unwrap();
        let cfg = Config {
            update: "box-update".into(),
            host: "box".into(),
            state: d.join("state"),
            work: d.join("work"),
            user: "sam".into(),
            home: home.to_str().unwrap().into(),
            uid: 1000,
            runuser: bin().join("runuser"),
            configuration: "box".into(),
            system: "x86_64-linux".into(),
            heavy: None,
            project: "homelab".into(),
            app: None,
            train: Some(train()),
        };
        let m = Machine { dir, cfg };
        let d = m.dir.path();
        m.git(d, &["init", "-q", "--bare", "origin.git"]);
        m.git(d, &["clone", "-q", "origin.git", "work"]);
        m.commit("flake.lock", "v1");
        m.work(&["push", "-q", "origin", "HEAD:main"]);
        m
    }

    pub fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    /// A file in the user's home, where the stand-ins keep what they did.
    pub fn home(&self, p: &str) -> PathBuf {
        Path::new(&self.cfg.home).join(p)
    }

    /// git in `at`, as the test, with the machine's user's config.
    pub fn git(&self, at: &Path, args: &[&str]) -> String {
        let out = self.git_at(at).args(args).output().unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    pub fn git_at(&self, at: &Path) -> Command {
        let mut c = Command::new("git");
        c.arg("-C")
            .arg(at)
            .env("HOME", &self.cfg.home)
            .env("GIT_CONFIG_NOSYSTEM", "1");
        c
    }

    pub fn work(&self, args: &[&str]) -> String {
        self.git(&self.cfg.work, args)
    }

    pub fn origin(&self, args: &[&str]) -> String {
        self.git(&self.path("origin.git"), args)
    }

    /// `file` holding `text`, committed in the clone with HOST-update's word
    /// on what it passed.
    pub fn commit(&self, file: &str, text: &str) {
        fs::write(self.cfg.work.join(file), format!("{text}\n")).unwrap();
        self.work(&["add", file]);
        self.work(&[
            "commit",
            "-qm",
            &format!("{file}: {text}"),
            "-m",
            "Pushed by box-update after the build, the VM tests and the health check passed.",
        ]);
    }

    /// `file` holding `text`, committed on GitHub's `branch` from a clone of
    /// its own (`by`), and pushed there: its new head.
    pub fn pushed(&self, by: &str, branch: &str, file: &str, text: &str) -> String {
        let other = self.path(by);
        let _ = fs::remove_dir_all(&other);
        let origin = self.path("origin.git");
        self.git(
            self.dir.path(),
            &["clone", "-q", "-b", branch, origin.to_str().unwrap(), by],
        );
        fs::write(other.join(file), format!("{text}\n")).unwrap();
        self.git(&other, &["add", file]);
        self.git(&other, &["commit", "-qm", &format!("{file}: {text}")]);
        self.git(
            &other,
            &["push", "-q", "--force", "origin", &format!("HEAD:{branch}")],
        );
        self.git(&other, &["rev-parse", "HEAD"])
    }

    /// main moving on GitHub, as another PR's landing would.
    pub fn landed(&self, file: &str, text: &str) {
        self.pushed("other", "main", file, text);
    }

    /// The clone as update.sh leaves it at the start of a run: at GitHub's
    /// main.
    pub fn at_main(&self) -> String {
        self.work(&["fetch", "-q", "origin", "main"]);
        self.work(&["checkout", "-q", "-B", "update", "origin/main"]);
        self.work(&["reset", "-q", "--hard", "origin/main"]);
        self.head()
    }

    pub fn head(&self) -> String {
        self.work(&["rev-parse", "HEAD"])
    }

    /// GitHub's main.
    pub fn main(&self) -> String {
        self.origin(&["rev-parse", "refs/heads/main"])
    }

    /// Whether the clone has `commit`.
    pub fn has(&self, commit: &str) -> bool {
        self.git_at(&self.cfg.work)
            .args(["cat-file", "-e", &format!("{commit}^{{tree}}")])
            .output()
            .unwrap()
            .status
            .success()
    }

    /// Whether the clone is as it was: at `at`, with nothing half done
    /// (a merge or a rebase) and nothing changed.
    pub fn left_at(&self, at: &str) -> bool {
        let git = self.cfg.work.join(".git");
        self.head() == at
            && !git.join("MERGE_HEAD").exists()
            && !git.join("rebase-merge").exists()
            && !git.join("rebase-apply").exists()
            && self.work(&["status", "--porcelain"]).is_empty()
    }

    /// What HOST-update recorded in its state file `name`, when there is
    /// one.
    pub fn recorded(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.cfg.state.join(name))
            .ok()
            .map(|s| s.trim_end().to_string())
    }

    /// What a stand-in kept in HOME/`name`: nothing when it kept nothing.
    pub fn kept(&self, name: &str) -> String {
        fs::read_to_string(self.home(name)).unwrap_or_default()
    }
}

pub fn sha(c: char) -> String {
    c.to_string().repeat(40)
}
