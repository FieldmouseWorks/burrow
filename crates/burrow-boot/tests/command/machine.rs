//! A machine made of files (systems with a kernel, an initrd and their
//! params, what booted and runs, GRUB's menu and grubenvs), with stand-ins
//! for what HOST-boot runs that keep how they were run in FAKE, a dir of
//! the machine's own.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

pub const BIN: &str = env!("CARGO_BIN_EXE_burrow-boot");

/// Written once for every test: a file being written while another test
/// starts a process is open for writing in that process until it execs,
/// and running the file then fails with "Text file busy". Each fails when
/// FAKE has a file saying so.
pub fn bin() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        for (name, text) in [
            (
                "sudo",
                // Never inside itself: a run it starts that starts it again
                // would never end.
                "#!/bin/sh\n[ -z \"$IN_SUDO\" ] || exit 9\necho \"$*\" >>\"$FAKE/sudo\"\n[ \"$1\" = -- ] || exit 9\nshift\nexport IN_SUDO=1\nexec \"$@\"\n",
            ),
            (
                "grub-editenv",
                "#!/bin/sh\necho \"$*\" >>\"$FAKE/grub-editenv\"\n[ ! -e \"$FAKE/fail-editenv-$2\" ] || exit 1\nd=${1%/grub/grubenv}\n[ ! -e \"$FAKE/fail-editenv-$2-${d##*/}\" ] || exit 1\ncase \"$2\" in\n  create) : >\"$1\" ;;\n  set) echo \"$3\" >>\"$1\" ;;\n  unset) echo \"-$3\" >>\"$1\" ;;\nesac\n",
            ),
            (
                "systemctl",
                "#!/bin/sh\necho \"$*\" >>\"$FAKE/systemctl\"\ncase \"$*\" in\n  --user*list-units*) cat \"$FAKE/jobs\" 2>/dev/null ;;\n  list-units*) cat \"$FAKE/sandboxes\" 2>/dev/null ;;\n  \"-q is-active sshd.service\") [ ! -e \"$FAKE/sshd-down\" ] ;;\n  reboot) [ ! -e \"$FAKE/fail-reboot\" ] ;;\nesac\n",
            ),
            (
                "box-notify",
                "#!/bin/sh\n(IFS='|'; echo \"$*\") >>\"$FAKE/notify\"\n[ ! -e \"$FAKE/fail-notify\" ]\n",
            ),
            (
                "systemd-cat",
                "#!/bin/sh\necho \"$* $(cat)\" >>\"$FAKE/journal\"\n",
            ),
            (
                // Every system's: which one it is is the dir it's in.
                "switch-to-configuration",
                "#!/bin/sh\nsys=${0%/bin/switch-to-configuration}\nname=${sys##*/}\necho \"$name $*\" >>\"$FAKE/switch-to-configuration\"\n[ ! -e \"$FAKE/fail-$name\" ]\n",
            ),
        ] {
            let path = bin.path().join(name);
            fs::write(&path, text).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        // For a run as nobody, too.
        fs::set_permissions(bin.path(), fs::Permissions::from_mode(0o755)).unwrap();
        bin
    })
    .path()
}

pub fn root() -> bool {
    rustix::process::geteuid().is_root()
}

pub struct Machine {
    _dir: tempfile::TempDir,
    /// The machine's dir, every symlink in it followed, as HOST-boot says
    /// its paths.
    root: PathBuf,
}

impl Machine {
    /// A machine that booted the system `one` and runs it, in a GRUB menu
    /// with both grub dirs, and nothing of HOST-boot's yet.
    pub fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let m = Machine { _dir: dir, root };
        for d in [
            "fake",
            "kernels",
            "systems",
            "run",
            "boot/grub",
            "boot-fallback/grub",
            "switch",
        ] {
            fs::create_dir_all(m.path(d)).unwrap();
        }
        m.system("one", "k1", "i1", "panic=10 loglevel=4\n");
        m.boots("one");
        symlink(m.path("systems/one"), m.path("run/current-system")).unwrap();
        m.menu(&["one"]);
        let b = bin();
        let config = serde_json::json!({
            "boot": "box-boot", "host": "box", "state": m.path("state"),
            "switchState": m.path("switch"), "user": "sam",
            "booted": m.path("run/booted-system"), "current": m.path("run/current-system"),
            "grubCfg": m.path("boot/grub/grub.cfg"),
            "grubDirs": [m.path("boot"), m.path("boot-fallback"), m.path("nowhere")],
            "tools": {"sudo": b.join("sudo"), "grubEditenv": b.join("grub-editenv"),
                      "systemctl": b.join("systemctl"), "notify": b.join("box-notify"),
                      "logger": b.join("systemd-cat")},
        });
        fs::write(m.path("config.json"), config.to_string()).unwrap();
        m
    }

    pub fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    /// systems/NAME, with its kernel and initrd (kernels/KERNEL and
    /// kernels/INITRD, made if they're new) and its kernel params.
    pub fn system(&self, name: &str, kernel: &str, initrd: &str, params: &str) -> PathBuf {
        let sys = self.path("systems").join(name);
        fs::create_dir_all(sys.join("bin")).unwrap();
        for k in [kernel, initrd] {
            let path = self.path("kernels").join(k);
            if !path.exists() {
                fs::write(&path, k).unwrap();
            }
        }
        symlink(self.path("kernels").join(kernel), sys.join("kernel")).unwrap();
        symlink(self.path("kernels").join(initrd), sys.join("initrd")).unwrap();
        fs::write(sys.join("kernel-params"), params).unwrap();
        symlink(
            bin().join("switch-to-configuration"),
            sys.join("bin/switch-to-configuration"),
        )
        .unwrap();
        sys
    }

    /// The machine booted systems/NAME.
    pub fn boots(&self, name: &str) {
        let booted = self.path("run/booted-system");
        let _ = fs::remove_file(&booted);
        symlink(self.path("systems").join(name), booted).unwrap();
    }

    /// GRUB's menu, as NixOS writes it: the first of `systems` as the
    /// default, then each of them in a submenu.
    pub fn menu(&self, systems: &[&str]) {
        let entry = |title: &str, name: &str| {
            format!(
                "menuentry \"{title}\" --class nixos {{\n  linux /kernels/k init={}/init loglevel=4\n}}\n",
                self.path("systems").join(name).display()
            )
        };
        let mut menu = String::from("set default=0\n");
        menu += &entry("NixOS - Default", systems[0]);
        menu += "submenu \"NixOS - All configurations\" --class submenu {\n";
        for name in systems {
            menu += &entry(&format!("NixOS - {name}"), name);
        }
        menu += "}\n";
        fs::write(self.path("boot/grub/grub.cfg"), menu).unwrap();
    }

    /// FAKE/NAME, which makes a stand-in fail or answer otherwise.
    pub fn set(&self, name: &str, text: &str) {
        fs::write(self.path("fake").join(name), text).unwrap();
    }

    /// HOST-boot with `words`, as anyone but root starts it.
    pub fn run(&self, words: &[&str]) -> Output {
        if root() {
            // Everything nobody's, for nobody's run: the machine's state
            // and the stand-ins' logs.
            chown(&self.root);
        }
        let mut c = Command::new(BIN);
        c.arg(self.path("config.json"))
            .args(words)
            .env("FAKE", self.path("fake"));
        if root() {
            c.uid(65534).gid(65534);
        }
        c.output().unwrap()
    }

    /// How the stand-in `name` was run, since it was last asked.
    pub fn fake(&self, name: &str) -> String {
        let path = self.path("fake").join(name);
        let text = fs::read_to_string(&path).unwrap_or_default();
        let _ = fs::remove_file(path);
        text
    }

    pub fn state(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.path("state").join(name)).ok()
    }

    pub fn sys(&self, name: &str) -> String {
        self.path("systems").join(name).display().to_string()
    }

    /// The boot bits of systems/NAME, as boot.sh's own shell hashed them.
    pub fn bits(&self, name: &str) -> String {
        let out = Command::new("sh")
            .arg("-c")
            .arg("printf '%s %s %s' \"$(readlink -f \"$1/kernel\")\" \"$(readlink -f \"$1/initrd\")\" \"$(cat \"$1/kernel-params\")\" | sha256sum | cut -d' ' -f1")
            .arg("sh")
            .arg(self.path("systems").join(name))
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
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

pub const USAGE: &str = "usage: box-boot default SYSTEM | trial [--force] | check | status\n";

/// A machine with `two`, a new kernel, queued for a trial a moment ago, and
/// in GRUB's menu.
pub fn queued() -> Machine {
    let m = Machine::new();
    m.system("two", "k2", "i1", "quiet");
    m.menu(&["one", "two"]);
    m.run(&["default", &m.sys("two")]);
    m.fake("switch-to-configuration");
    m.fake("sudo");
    m
}
