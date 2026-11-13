//! A machine made of files: systems with their switch-to-configuration and
//! inputs.json, what runs and the system profile, and stand-ins for what
//! HOST-switch and HOST-confirm run, which keep how they were run in FAKE.
//!
//! The stand-ins act as much as the tests need: switch-to-configuration
//! `test` makes its system the one that runs, systemd-run arms the revert's
//! timer and runs what it's told to wait for, nix-env points the profile.
//! A file in FAKE makes one fail or answer otherwise.

use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

pub const SWITCH: &str = env!("CARGO_BIN_EXE_burrow-switch");
pub const CONFIRM: &str = env!("CARGO_BIN_EXE_burrow-confirm");

/// Shared stand-ins, installed by burrow_test and cached for the test binary.
pub fn bin() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        // The tests' own PATH for the stand-ins, whatever PATH they're
        // run with: theirs is the switch's business.
        let path = std::env::var("PATH").unwrap();
        for (name, text) in STAND_INS {
            let file = bin.path().join(name);
            burrow_test::executable(&file, text.replace("@PATH@", &path));
        }
        // The PATH the config gives units and systemd-cat: cat, and
        // nothing else.
        fs::create_dir(bin.path().join("path")).unwrap();
        let cat = std::env::split_paths(&path)
            .map(|d| d.join("cat"))
            .find(|c| c.exists())
            .unwrap();
        symlink(cat, bin.path().join("path/cat")).unwrap();
        // For a run as nobody, too.
        burrow_test::permissions(bin.path(), 0o755);
        bin
    })
    .path()
}

const STAND_INS: [(&str, &str); 11] = [
    (
        "sudo",
        // Never inside itself: a run it starts that starts it again would
        // never end. Like sudo, it keeps none of the environment it was
        // started with, but the tests' own and SERVICE_RESULT, which a
        // unit gives the switch as root, where there's no sudo.
        r#"#!/bin/sh
given=$PATH PATH='@PATH@'
[ -z "$IN_SUDO" ] || exit 9
echo "$*" >>"$FAKE/sudo"
[ "$1" = -- ] || exit 9
shift
exec env -i FAKE="$FAKE" CURRENT="$CURRENT" LISTS="$LISTS" PATH="$given" ${SERVICE_RESULT+SERVICE_RESULT="$SERVICE_RESULT"} IN_SUDO=1 "$@"
"#,
    ),
    (
        // The revert is activating (or FAKE/activating-as) for as many
        // looks as FAKE/activating-for says (from when the timer's stopped,
        // with FAKE/fires-on-stop), and FAKE/lands runs at the last of
        // them, if there's one.
        "systemctl",
        r#"#!/bin/sh
PATH='@PATH@'
echo "$*" >>"$FAKE/systemctl"
case "$*" in
  "show -p ActiveState --value box-revert.service")
    n=$(cat "$FAKE/activating-for" 2>/dev/null || echo 0)
    if [ "$n" -gt 0 ]; then
      echo $((n - 1)) >"$FAKE/activating-for"
      [ "$n" -gt 1 ] || [ ! -e "$FAKE/lands" ] || sh "$FAKE/lands"
      cat "$FAKE/activating-as" 2>/dev/null || echo activating
    else
      cat "$FAKE/revert-state" 2>/dev/null || echo inactive
    fi ;;
  "show -p SubState --value box-revert.timer") cat "$FAKE/timer-state" 2>/dev/null || echo dead ;;
  "list-timers box-revert.timer --no-pager")
    printf 'NEXT LEFT LAST PASSED UNIT ACTIVATES\nMon 10:10 9min - - box-revert.timer box-revert.service\n\n1 timers listed.\n' ;;
  "stop box-revert.timer")
    [ -e "$FAKE/keeps-timer" ] || echo dead >"$FAKE/timer-state"
    [ ! -e "$FAKE/fires-on-stop" ] || mv "$FAKE/fires-on-stop" "$FAKE/activating-for" ;;
esac
"#,
    ),
    (
        // A timer armed waits; what it's told to wait for, it runs, with
        // the PATH it's given or a bare one, as systemd does. FAKE/fail-run-
        // UNIT is how many runs of UNIT fail: every one, when it's empty.
        "systemd-run",
        r#"#!/bin/sh
PATH='@PATH@'
echo "$*" >>"$FAKE/systemd-run"
wait= path=/var/empty
for a in "$@"; do
  case $a in
    --unit=*)
      f=$FAKE/fail-run-${a#--unit=}
      if [ -e "$f" ]; then
        n=$(cat "$f")
        case $n in
          '') exit 1 ;;
          0) ;;
          *) echo $((n - 1)) >"$f"; exit 1 ;;
        esac
      fi ;;
    --on-active=*) echo waiting >"$FAKE/timer-state" ;;
    --setenv=PATH=*) path=${a#--setenv=PATH=} ;;
    --wait) wait=1 ;;
  esac
done
[ -n "$wait" ] || exit 0
while [ "${1#--}" != "$1" ]; do shift; done
# A unit of its own, as root: a switch it runs isn't inside the sudo that
# started this one.
unset IN_SUDO
PATH=$path exec "$@"
"#,
    ),
    (
        // systemd's job queue: FAKE/jobs is what ListJobs says, FAKE/before-ID
        // what job ID waits for (nothing, when they aren't there). A job's
        // Cancel and a unit's job queued again are kept in FAKE/busctl;
        // FAKE/fail-cancel fails a cancel, as for a job that just ended.
        "busctl",
        r#"#!/bin/sh
PATH='@PATH@'
[ "$1 $2 $3" = "--json=short call org.freedesktop.systemd1" ] || exit 1
shift 3
reply() { printf '{"type":"a(usssoo)","data":[[%s]]}\n' "$1"; }
case "$2 $3" in
  "org.freedesktop.systemd1.Manager ListJobs") reply "$(cat "$FAKE/jobs" 2>/dev/null)" ;;
  "org.freedesktop.systemd1.Job GetBefore") reply "$(cat "$FAKE/before-${1##*/}" 2>/dev/null)" ;;
  "org.freedesktop.systemd1.Job Cancel")
    if [ -e "$FAKE/fail-cancel" ]; then echo "$1" >"$FAKE/cancel-failed"; exit 1; fi
    echo "cancel ${1##*/}" >>"$FAKE/busctl" ;;
  "org.freedesktop.systemd1.Manager "*Unit)
    echo "$3 $5 $6" >>"$FAKE/busctl"
    echo '{"type":"o","data":["/org/freedesktop/systemd1/job/900"]}' ;;
  *) exit 1 ;;
esac
"#,
    ),
    (
        "systemd-analyze",
        r#"#!/bin/sh
PATH='@PATH@'
[ "$1" = timespan ] || exit 1
case $2 in
  *min) n=${2%min}; m=60000000 ;;
  *s) n=${2%s}; m=1000000 ;;
  *) echo "Failed to parse time span '$2'" >&2; exit 1 ;;
esac
case $n in ''|*[!0-9]*) exit 1 ;; esac
printf 'Original: %s\n      μs: %s\n   Human: %s\n' "$2" $((n * m)) "$2"
"#,
    ),
    (
        // As systemd-cat with no command: what it's given goes to the
        // journal through cat, found on the PATH it's run with.
        "systemd-cat",
        r#"#!/bin/sh
[ ! -e "$FAKE/fail-logger" ] || exit 1
printf '%s ' "$*" >>"$FAKE/journal"
exec cat >>"$FAKE/journal"
"#,
    ),
    (
        "runuser",
        r#"#!/bin/sh
PATH='@PATH@'
echo "$*" >>"$FAKE/runuser"
while [ "$1" != -- ]; do shift; done
shift
exec "$@"
"#,
    ),
    (
        "nix",
        r#"#!/bin/sh
PATH='@PATH@'
echo "$*" >>"$FAKE/nix"
[ ! -e "$FAKE/fail-nix" ] || exit 1
cat "$FAKE/built"
"#,
    ),
    (
        "nix-env",
        r#"#!/bin/sh
PATH='@PATH@'
echo "NIX_REMOTE=$NIX_REMOTE $*" >>"$FAKE/nix-env"
[ ! -e "$FAKE/fail-nix-env" ] || exit 1
ln -sfn "$4" "$2"
"#,
    ),
    (
        "box-boot",
        r#"#!/bin/sh
PATH='@PATH@'
echo "$*" >>"$FAKE/box-boot"
[ ! -e "$FAKE/fail-boot" ]
"#,
    ),
    (
        // Every system's: which one it is is the dir it's in. `test` makes
        // it the one that runs, unless it hangs (the revert fires) or stays.
        // FAKE/locked-NAME is how many tries it refuses as locked. With
        // FAKE/lists, it lists what it restarts as FAKE/lists says, and waits
        // (up to 20s) for one of its jobs to be let go (or tried), as a real one waits
        // for its jobs.
        "switch-to-configuration",
        r#"#!/bin/sh
PATH='@PATH@'
sys=${0%/bin/switch-to-configuration}
name=${sys##*/}
echo "$name $*" >>"$FAKE/switch-to-configuration"
if [ -e "$FAKE/lists" ]; then
  cp "$FAKE/lists" "$LISTS/restart-list"
  n=0
  until grep -q '^cancel ' "$FAKE/busctl" 2>/dev/null || [ -e "$FAKE/cancel-failed" ] || [ $n -ge 200 ]; do sleep 0.1; n=$((n + 1)); done
  rm -f "$LISTS/restart-list"
fi
n=$(cat "$FAKE/locked-$name" 2>/dev/null || echo 0)
if [ "$n" -gt 0 ]; then echo $((n - 1)) >"$FAKE/locked-$name"; exit 11; fi
if [ -e "$FAKE/hang-$name" ]; then echo dead >"$FAKE/timer-state"; exit 1; fi
[ "$1" != test ] || [ -e "$FAKE/stays-$name" ] || ln -sfn "$sys" "$CURRENT"
exit $(cat "$FAKE/exit-$name" 2>/dev/null || echo 0)
"#,
    ),
];

/// The PATH the config gives the revert's units and systemd-cat.
pub fn unit_path() -> String {
    bin().join("path").display().to_string()
}

pub fn root() -> bool {
    rustix::process::geteuid().is_root()
}

pub struct Machine {
    _dir: tempfile::TempDir,
    /// The machine's dir, every symlink in it followed, as the switch says
    /// its paths.
    pub root: PathBuf,
}

impl Machine {
    /// A machine running `one`, its confirmed system, with `two` built
    /// from a newer nixpkgs, and nothing pending. Without trial boots
    /// unless `boot`.
    pub fn new(boot: bool) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let m = Machine { _dir: dir, root };
        for d in ["fake", "systems", "run", "flake", "lists"] {
            fs::create_dir_all(m.path(d)).unwrap();
        }
        m.system("one", Some(100));
        m.system("two", Some(200));
        symlink(m.path("systems/one"), m.path("run/current-system")).unwrap();
        symlink(m.path("systems/one"), m.path("profile")).unwrap();
        let b = bin();
        let config = serde_json::json!({
            "switch": "box-switch", "confirm": "box-confirm", "revert": "box-revert",
            "state": m.path("state"), "flake": m.path("flake"), "configuration": "box",
            "within": "10min", "current": m.path("run/current-system"), "profile": m.path("profile"),
            "activationLock": m.path("s2c.lock"), "activationLists": m.path("lists"),
            "path": unit_path(),
            "tools": {"sudo": b.join("sudo"), "systemctl": b.join("systemctl"), "busctl": b.join("busctl"),
                      "systemdRun": b.join("systemd-run"), "systemdAnalyze": b.join("systemd-analyze"),
                      "logger": b.join("systemd-cat"), "runuser": b.join("runuser"),
                      "nix": b.join("nix"), "nixEnv": b.join("nix-env"),
                      "boot": if boot { Some(b.join("box-boot")) } else { None }},
        });
        fs::write(m.path("config.json"), config.to_string()).unwrap();
        m
    }

    pub fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    /// systems/NAME, whose inputs.json says when its nixpkgs is from, if
    /// it says.
    pub fn system(&self, name: &str, nixpkgs: Option<u64>) -> String {
        let sys = self.path("systems").join(name);
        fs::create_dir_all(sys.join("bin")).unwrap();
        fs::create_dir_all(sys.join("etc/burrow")).unwrap();
        if let Some(t) = nixpkgs {
            fs::write(
                sys.join("etc/burrow/inputs.json"),
                serde_json::json!({"nixpkgs": {"rev": "abc", "lastModified": t}}).to_string(),
            )
            .unwrap();
        }
        symlink(
            bin().join("switch-to-configuration"),
            sys.join("bin/switch-to-configuration"),
        )
        .unwrap();
        self.sys(name)
    }

    pub fn sys(&self, name: &str) -> String {
        self.path("systems").join(name).display().to_string()
    }

    /// What runs now, and what the profile says.
    pub fn running(&self) -> String {
        fs::canonicalize(self.path("run/current-system"))
            .unwrap()
            .display()
            .to_string()
    }

    pub fn runs(&self, name: &str) {
        let current = self.path("run/current-system");
        fs::remove_file(&current).unwrap();
        symlink(self.path("systems").join(name), current).unwrap();
    }

    pub fn profile(&self) -> String {
        fs::canonicalize(self.path("profile"))
            .unwrap()
            .display()
            .to_string()
    }

    /// FAKE/NAME, which makes a stand-in fail or answer otherwise.
    pub fn set(&self, name: &str, text: &str) {
        fs::write(self.path("fake").join(name), text).unwrap();
    }

    /// `bin` with `words`, as anyone but root starts it: as nobody, when
    /// the tests run as root, so it goes through sudo wherever they run.
    pub fn command(&self, bin: &str, words: &[&str]) -> Command {
        self.command_with(&self.path("config.json"), bin, words)
    }

    /// The same, with the config at `config`.
    pub fn command_with(&self, config: &Path, bin: &str, words: &[&str]) -> Command {
        if root() {
            // Everything nobody's, for nobody's run.
            chown(&self.root);
        }
        let mut c = Command::new(bin);
        c.arg(config)
            .args(words)
            .env("FAKE", self.path("fake"))
            .env("CURRENT", self.path("run/current-system"))
            .env("LISTS", self.path("lists"))
            .env_remove("BURROW_CONFIRM_WITHIN")
            .env_remove("SERVICE_RESULT");
        if root() {
            c.uid(65534).gid(65534);
        }
        c
    }

    pub fn switch(&self, words: &[&str]) -> Output {
        self.command(SWITCH, words).output().unwrap()
    }

    pub fn confirm(&self) -> Output {
        self.command(CONFIRM, &[]).output().unwrap()
    }

    /// How the stand-in `name` was run, since it was last asked.
    pub fn fake(&self, name: &str) -> String {
        let path = self.path("fake").join(name);
        let text = fs::read_to_string(&path).unwrap_or_default();
        let _ = fs::remove_file(path);
        text
    }

    /// The state file `name`, if there's one.
    pub fn state(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.path("state").join(name)).ok()
    }

    /// How a unit runs the switch itself: its binary and its config.
    pub fn me(&self) -> String {
        format!(
            "{} {}",
            fs::canonicalize(SWITCH).unwrap().display(),
            self.path("config.json").display()
        )
    }

    /// The machine as a switch to `two` left it, live and unconfirmed,
    /// with nothing it ran remembered.
    pub fn switched(boot: bool) -> Machine {
        let m = Machine::new(boot);
        let out = m.switch(&["--toplevel", &m.sys("two")]);
        assert_eq!(out.status.code(), Some(0), "{}", said(&out).2);
        for f in [
            "sudo",
            "systemctl",
            "systemd-run",
            "switch-to-configuration",
            "journal",
        ] {
            m.fake(f);
        }
        m
    }
}

pub fn chown(path: &Path) {
    std::os::unix::fs::lchown(path, Some(65534), Some(65534)).unwrap();
    if path.is_dir() && !path.is_symlink() {
        for entry in fs::read_dir(path).unwrap() {
            chown(&entry.unwrap().path());
        }
    }
}

pub fn said(out: &Output) -> (Option<i32>, String, String) {
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

pub fn ok(out: &str) -> (Option<i32>, String, String) {
    (Some(0), out.into(), String::new())
}

pub fn refused(code: i32, err: &str) -> (Option<i32>, String, String) {
    (Some(code), String::new(), err.into())
}
