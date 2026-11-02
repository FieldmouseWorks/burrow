//! A machine for the steps' tests: its clone of the flake at main, GitHub's
//! copy beside it as a bare repo, HOST-update's state, and stand-ins for
//! what the steps run as the machine's user.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use burrow_github::{Failed, Response};
use serde_json::{Value, json};

use crate::config::Tools;
use crate::{Config, Train};

/// The branch its PR comes from, in a train repo.
pub const BRANCH: &str = "box-update";

/// The stand-ins, written once for every test: a file being written while
/// another test starts a process is open for writing in that process until
/// it execs, and running the file then fails with "Text file busy". What a
/// run runs as the machine's user (through runuser, and so PATH) keeps what
/// a test needs to see under the HOME it runs with, the test's own; what it
/// runs as root, by path, is a link in the test's own bin (`Machine::tool`),
/// and keeps it beside that bin, in the test's dir (T below).
///
/// As the user:
///
/// - runuser runs what it's given as it is, as the machine's user would,
///   with the PATH it's given (these first, then the test's); before a push
///   from the clone WORK it runs
///   the test's hook, WORK.before-push, when there is one (a hook that's to
///   run once removes itself).
/// - nix logs each call to HOME/nix.log, and fails as a failed build does
///   with HOME/nix-down there, or says nothing and goes with
///   HOME/nix-says-nothing there. A system it builds is made of flake.lock,
///   system.nix and claude-code.json, and stamped with the commit it's built
///   from, as a real one is (a README, say, goes into none of them): HEAD's as a run asks for it
///   (WORK#...toplevel, stamped with HEAD), or a commit's as
///   same_as_running does (an --expr with git+file://WORK?rev=COMMIT,
///   stamped with what it forces). It's a dir in T/store, with that
///   flake.lock and claude-code.json in it and a sw/bin/nixos-version that
///   says its stamp. Each
///   tree it builds goes to HOME/built, and one whose flake.lock has a line
///   of HOME/build-breaks in it doesn't build. A VM test (#checks.) says
///   what it ran on stdout and stderr both, and fails with HOME/check-fails
///   there, on a commit whose flake.lock has a line of HOME/check-breaks in
///   it, or once, with HOME/check-flaky there. Its flake's metadata is
///   WORK/flake.lock's (a lock as `lock` writes one), and `flake update
///   INPUT` locks INPUT to HOME/upstream-INPUT's commit, when there is one,
///   or fails, with HOME/unfetchable-INPUT there.
/// - heavy takes a slot (HOME/nix.log says so) and runs what follows `--`.
/// - proj logs each call to HOME/proj.log and says its job's id, unless
///   HOME/proj-down is there: then it fails.
/// - claude says its version, the claude-code.json of the system that runs
///   (T/current-system), and answers a prompt (logged in HOME/claude.log)
///   with OK, unless HOME/claude-mute is there.
///
/// As root, each logging what it's asked to T/calls:
///
/// - switch puts T/current-system at the system it's given, keeping the
///   one before (T/previous) for --abort to go back to, and exits with
///   T/switch-rc's code when there is one; confirm fails with
///   T/confirm-fails there.
/// - health snapshots, and compares unhealthy, saying T/unhealthy, when
///   that's there.
/// - notify keeps what it opens and closes in T/notices, and each notice's
///   body in T/notice-KEY.
/// - vulnix reports, for a system, each line "COMMIT CVE PACKAGE SCORE" of
///   T/vulnix-db whose commit its flake.lock has; it says T/vulnix-says
///   instead when that's there, and fails with T/vulnix-down there.
/// - curl fetches T/releases/PATH for downloads.example/claude/PATH.
/// - install makes dirs, and copies files, without the owner it's given.
/// - systemd-cat keeps what it's given in T/journal; systemctl lists two
///   timers; boot says T/boot-status; sudo runs what follows `--`.
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
case $4 in PATH=*) ;; *) exit 9 ;; esac
if [ "$5 $6 $8" = "git -C push" ] && [ -e "$7.before-push" ]; then
  sh "$7.before-push"
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
breaks() {
  [ -s "$HOME/$1" ] && git -C "$2" show "$3:flake.lock" 2>/dev/null | grep -qF -f "$HOME/$1"
}
case "$1 $2" in
  "flake metadata")
    printf '{"locks": '
    cat "$4/flake.lock"
    echo '}'
    exit ;;
  "flake update")
    [ ! -e "$HOME/unfetchable-$3" ] || { echo "error: unable to download $3" >&2; exit 1; }
    [ -e "$HOME/upstream-$3" ] || exit 0
    sed -i "s|^\"$3\": {\"locked\": {\"rev\": \"[0-9a-f]*\"}}|\"$3\": {\"locked\": {\"rev\": \"$(cat "$HOME/upstream-$3")\"}}|" "$5/flake.lock"
    exit ;;
esac
case "$*" in *'#checks.'*)
  echo "ran $*"
  echo "a VM test's log" >&2
  [ ! -e "$HOME/check-fails" ] || exit 1
  if [ -e "$HOME/check-flaky" ]; then rm "$HOME/check-flaky"; exit 1; fi
  ref=$(printf '%s\n' "$*" | sed -n 's|.*git+file://\([^?]*\)?rev=\([0-9a-f]*\)#.*|\1 \2|p')
  ! breaks check-breaks ${ref% *} ${ref#* }
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
! breaks build-breaks "$dir" "$commit" || { echo "error: build failed" >&2; exit 1; }
sys=${HOME%/home}/store/$(printf '%s\n' "$(git -C "$dir" ls-tree "$commit" -- flake.lock system.nix claude-code.json)" "$stamp" | sha256sum | cut -c1-32)-nixos-system
if [ ! -d "$sys" ]; then
  mkdir -p "$sys/sw/bin"
  git -C "$dir" show "$commit:flake.lock" >"$sys/flake.lock" 2>/dev/null || :
  git -C "$dir" show "$commit:claude-code.json" >"$sys/claude-code.json" 2>/dev/null || rm -f "$sys/claude-code.json"
  rev=$(printf '%s' "$stamp" | tr -d '"')
  [ "$rev" != null ] || rev=
  printf '#!/bin/sh\n[ "$1" = --configuration-revision ] || exit 9\necho %s\n' "$rev" >"$sys/sw/bin/nixos-version"
  chmod +x "$sys/sw/bin/nixos-version"
fi
echo "$sys"
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
            (
                "claude",
                r#"#!/bin/sh
case $1 in
  --version) cat "${HOME%/home}/current-system/claude-code.json" 2>/dev/null ;;
  -p) printf '%s\n' "$*" >>"$HOME/claude.log"; [ -e "$HOME/claude-mute" ] || echo OK ;;
  *) exit 9 ;;
esac
"#,
            ),
            (
                "switch",
                r#"#!/bin/sh
T=${0%/*}/..
printf 'switch %s\n' "$*" >>"$T/calls"
case $1 in
  --abort)
    [ ! -e "$T/previous" ] || ln -sfn "$(cat "$T/previous")" "$T/current-system"
    echo "taken back"
    exit 0 ;;
  --toplevel)
    echo "switching to $2, to confirm within $BURROW_CONFIRM_WITHIN"
    echo "a switch's own words" >&2
    rc=$(cat "$T/switch-rc" 2>/dev/null || echo 0)
    [ "$rc" != 3 ] || exit 3
    readlink -f "$T/current-system" >"$T/previous"
    ln -sfn "$2" "$T/current-system"
    exit "$rc" ;;
esac
exit 9
"#,
            ),
            (
                "confirm",
                r#"#!/bin/sh
T=${0%/*}/..
echo confirm >>"$T/calls"
echo confirmed
[ ! -e "$T/confirm-fails" ]
"#,
            ),
            (
                "health",
                r#"#!/bin/sh
T=${0%/*}/..
printf 'health %s\n' "$*" >>"$T/calls"
case $1 in
  snapshot) [ ! -e "$T/snapshot-fails" ] && echo healthy >"$2" ;;
  compare)
    [ -e "$2" ] || exit 9
    [ ! -e "$T/unhealthy" ] || { cat "$T/unhealthy"; exit 1; } ;;
  *) exit 9 ;;
esac
"#,
            ),
            (
                "notify",
                r#"#!/bin/sh
T=${0%/*}/..
case $1 in
  open) printf 'open %s: %s\n' "$2" "$3" >>"$T/notices"; printf '%s\n' "$4" >"$T/notice-$2" ;;
  close) printf 'close %s: %s\n' "$2" "$3" >>"$T/notices" ;;
  *) exit 9 ;;
esac
"#,
            ),
            (
                "vulnix",
                r#"#!/bin/sh
T=${0%/*}/..
[ "$1 $2" = "--closure --json" ] || exit 9
printf 'vulnix %s\n' "$3" >>"$T/calls"
[ ! -e "$T/vulnix-down" ] || exit 1
[ ! -e "$T/vulnix-says" ] || { cat "$T/vulnix-says"; exit 0; }
cat "$T/vulnix-db" 2>/dev/null | {
  printf '['
  sep=
  while read -r rev cve pkg score; do
    grep -qF "$rev" "$3/flake.lock" 2>/dev/null || continue
    printf '%s{"pname": "%s", "affected_by": ["%s"], "cvssv3_basescore": {"%s": %s}}' "$sep" "$pkg" "$cve" "$cve" "$score"
    sep=,
  done
  echo ']'
}
exit 2
"#,
            ),
            (
                "curl",
                r#"#!/bin/sh
T=${0%/*}/..
[ "$1 $2" = "-fsS --max-time" ] || exit 9
printf 'curl %s %s\n' "$3" "$4" >>"$T/calls"
file=$T/releases/${4#https://downloads.example/claude/}
[ -e "$file" ] || { echo "curl: (22) The requested URL returned error: 404" >&2; exit 22; }
if [ "$5" = -o ]; then cp "$file" "$6"; else cat "$file"; fi
"#,
            ),
            (
                "install",
                r#"#!/bin/sh
T=${0%/*}/..
printf 'install %s\n' "$*" >>"$T/calls"
if [ "$1" = -d ]; then mkdir -p "$6"; else cp "$7" "$8"; fi
"#,
            ),
            (
                "systemd-cat",
                r#"#!/bin/sh
T=${0%/*}/..
{ printf '%s: ' "$*"; cat; } >>"$T/journal"
"#,
            ),
            (
                "systemctl",
                r#"#!/bin/sh
T=${0%/*}/..
printf 'systemctl %s\n' "$*" >>"$T/calls"
echo "NEXT                        LEFT LAST PASSED UNIT                          ACTIVATES"
echo "Sun 2026-11-01 10:00:00 PST 5min -    -      burrow-events-catch-up.timer burrow-events-catch-up.service"
echo "Mon 2026-11-02 03:00:00 PST 17h  -    -      box-boot-trial.timer         box-boot-trial.service"
echo
echo "2 timers listed."
"#,
            ),
            (
                "boot",
                r#"#!/bin/sh
T=${0%/*}/..
[ "$1" = status ] || exit 9
cat "$T/boot-status" 2>/dev/null || echo "no trial queued"
"#,
            ),
            (
                "sudo",
                r#"#!/bin/sh
T=${0%/*}/..
echo sudo >>"$T/calls"
[ "$1" = -- ] || exit 9
shift
exec "$@"
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

/// The stand-in `standin`, as the run runs it as root: `name` in the
/// test's own bin, a link to the one every test shares, which keeps what it
/// does beside that bin.
fn tool(dir: &Path, standin: &str, name: &str) -> PathBuf {
    let path = dir.join("bin").join(name);
    std::os::unix::fs::symlink(bin().join(standin), &path).unwrap();
    path
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
        fs::create_dir(d.join("bin")).unwrap();
        fs::create_dir(d.join("switch")).unwrap();
        let cfg = Config {
            update: "box-update".into(),
            host: "box".into(),
            state: d.join("state"),
            work: d.join("work"),
            user: "sam".into(),
            home: home.to_str().unwrap().into(),
            uid: 1000,
            runuser: bin().join("runuser"),
            path: format!(
                "{}:{}",
                bin().display(),
                std::env::var("PATH").unwrap_or_default()
            ),
            configuration: "box".into(),
            system: "x86_64-linux".into(),
            heavy: None,
            project: "homelab".into(),
            app: None,
            train: Some(train()),
            origin: d.join("origin.git").to_str().unwrap().into(),
            flake: d.join("operator"),
            group: "users".into(),
            owner: "Sam".into(),
            manifest: None,
            releases: "https://downloads.example/claude".into(),
            checks: Vec::new(),
            within: "15min".into(),
            switch_state: d.join("switch"),
            current_system: d.join("current-system"),
            lock: d.join("update.lock"),
            timers: vec!["burrow-events-catch-up.timer".into()],
            tools: Tools {
                sudo: tool(&d, "sudo", "sudo"),
                switch: tool(&d, "switch", "box-switch"),
                confirm: tool(&d, "confirm", "box-confirm"),
                boot: None,
                health: tool(&d, "health", "box-health"),
                notify: tool(&d, "notify", "box-notify"),
                vulnix: tool(&d, "vulnix", "vulnix"),
                curl: tool(&d, "curl", "curl"),
                install: tool(&d, "install", "install"),
                logger: tool(&d, "systemd-cat", "systemd-cat"),
                systemctl: tool(&d, "systemctl", "systemctl"),
            },
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

    /// The clone as a run leaves it when it begins: at GitHub's
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

/// GitHub's list of open PRs: each one's number, head and head's repo.
pub fn listed(prs: &[(u64, String, Value)]) -> Value {
    prs.iter()
        .map(|(n, sha, repo)| {
            let repo = if repo.is_null() {
                Value::Null
            } else {
                json!({"full_name": repo})
            };
            json!({"number": n, "head": {"sha": sha, "ref": BRANCH, "repo": repo}})
        })
        .collect()
}

/// GitHub gone, as gh says it.
pub fn down() -> Result<Response, Failed> {
    Err(Failed {
        why: "gh: error connecting to api.github.com".into(),
        transient: true,
    })
}

/// GraphQL's answer for a PR whose head is `head`, with these checks; with
/// `more`, they have a page more.
pub fn checked(head: &str, checks: &[Value], more: bool) -> Value {
    let (suites, contexts): (Vec<&Value>, Vec<&Value>) =
        checks.iter().partition(|c| c["__typename"] == "CheckSuite");
    let suites: Vec<Value> = suites
        .into_iter()
        .map(|s| json!({"conclusion": s["conclusion"]}))
        .collect();
    json!({"data": {"repository": {"pullRequest": {"headRefOid": head, "commits": {"nodes": [{"commit": {
        "statusCheckRollup": {"contexts": {"pageInfo": {"hasNextPage": more}, "nodes": contexts}},
        "checkSuites": {"pageInfo": {"hasNextPage": false}, "nodes": suites}}}]}}}}})
}

/// A flake.lock with each of `inputs` locked to its commit, a node to a
/// line, as nix's stand-in reads it and bumps it (`bin`).
pub fn lock(inputs: &[(&str, &str)]) -> String {
    let root: Vec<String> = inputs
        .iter()
        .map(|(i, _)| format!("\"{i}\": \"{i}\""))
        .collect();
    let mut lines = vec![format!(
        "{{\"nodes\": {{\n\"root\": {{\"inputs\": {{{}}}}}",
        root.join(", ")
    )];
    for (i, rev) in inputs {
        lines.push(format!("\"{i}\": {{\"locked\": {{\"rev\": \"{rev}\"}}}}"));
    }
    lines.join(",\n") + "\n}}"
}

impl Machine {
    /// The machine as a run finds it, outside a train with one VM test,
    /// vm: main's flake locking `inputs`, built and running, stamped with
    /// main, and the operator's checkout of it at main.
    pub fn running_main(inputs: &[(&str, &str)]) -> Machine {
        let mut m = Machine::new();
        m.cfg.train = None;
        m.cfg.checks = vec!["vm".into()];
        m.commit("flake.lock", &lock(inputs));
        m.work(&["push", "-q", "origin", "HEAD:main"]);
        m.at_main();
        let system = crate::running::build(&m.cfg).unwrap();
        m.runs(&system);
        m.git(m.dir.path(), &["clone", "-q", "origin.git", "operator"]);
        fs::remove_file(m.home("built")).unwrap();
        fs::remove_file(m.home("nix.log")).unwrap();
        m
    }

    /// The machine, running `system`.
    pub fn runs(&self, system: &str) {
        let _ = fs::remove_file(&self.cfg.current_system);
        std::os::unix::fs::symlink(system, &self.cfg.current_system).unwrap();
    }

    /// The system it runs.
    pub fn running(&self) -> String {
        fs::canonicalize(&self.cfg.current_system)
            .unwrap()
            .to_str()
            .unwrap()
            .into()
    }

    /// A system of its own, stamped `rev`, that no build made: one someone
    /// switched to by hand. Written by a shell of its own, never this
    /// process (`bin`).
    pub fn system(&self, name: &str, rev: &str) -> String {
        let sys = self.path("store").join(format!("{name}-nixos-system"));
        let made = Command::new("sh")
            .arg("-c")
            .arg(r#"mkdir -p "$1/sw/bin" && printf '#!/bin/sh\necho %s\n' "$2" >"$1/sw/bin/nixos-version" && chmod +x "$1/sw/bin/nixos-version""#)
            .arg("sh")
            .arg(&sys)
            .arg(rev)
            .status()
            .unwrap();
        assert!(made.success());
        sys.to_str().unwrap().into()
    }

    /// `input` upstream, at `rev`: what a bump brings in.
    pub fn upstream(&self, input: &str, rev: &str) {
        fs::write(self.home(&format!("upstream-{input}")), rev).unwrap();
    }

    /// `name` in the test's dir, where what the run runs as root keeps
    /// what it did (`bin`): nothing when it kept nothing.
    pub fn did(&self, name: &str) -> String {
        fs::read_to_string(self.path(name)).unwrap_or_default()
    }

    /// What the last run kept in status.json.
    pub fn status(&self) -> serde_json::Value {
        serde_json::from_slice(&fs::read(self.cfg.state.join("status.json")).unwrap()).unwrap()
    }
}
