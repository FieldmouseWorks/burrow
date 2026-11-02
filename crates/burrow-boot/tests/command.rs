//! HOST-boot as the machine runs it: burrow-boot CONFIG WORD, on a machine
//! made of files (systems with a kernel, an initrd and their params, what
//! booted and runs, GRUB's menu and grubenvs), with stand-ins for what it
//! runs that keep how they were run in FAKE, a dir of the machine's own.
//!
//! Every run is as someone other than root (as nobody, when the tests run
//! as root), so it goes through sudo wherever the tests run.

use std::fs::{self, File};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

const BIN: &str = env!("CARGO_BIN_EXE_burrow-boot");

/// Written once for every test: a file being written while another test
/// starts a process is open for writing in that process until it execs,
/// and running the file then fails with "Text file busy". Each fails when
/// FAKE has a file saying so.
fn bin() -> &'static Path {
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

fn root() -> bool {
    rustix::process::geteuid().is_root()
}

struct Machine {
    _dir: tempfile::TempDir,
    /// The machine's dir, every symlink in it followed, as HOST-boot says
    /// its paths.
    root: PathBuf,
}

impl Machine {
    /// A machine that booted the system `one` and runs it, in a GRUB menu
    /// with both grub dirs, and nothing of HOST-boot's yet.
    fn new() -> Machine {
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

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    /// systems/NAME, with its kernel and initrd (kernels/KERNEL and
    /// kernels/INITRD, made if they're new) and its kernel params.
    fn system(&self, name: &str, kernel: &str, initrd: &str, params: &str) -> PathBuf {
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
    fn boots(&self, name: &str) {
        let booted = self.path("run/booted-system");
        let _ = fs::remove_file(&booted);
        symlink(self.path("systems").join(name), booted).unwrap();
    }

    /// GRUB's menu, as NixOS writes it: the first of `systems` as the
    /// default, then each of them in a submenu.
    fn menu(&self, systems: &[&str]) {
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
    fn set(&self, name: &str, text: &str) {
        fs::write(self.path("fake").join(name), text).unwrap();
    }

    /// HOST-boot with `words`, as anyone but root starts it.
    fn run(&self, words: &[&str]) -> Output {
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
    fn fake(&self, name: &str) -> String {
        let path = self.path("fake").join(name);
        let text = fs::read_to_string(&path).unwrap_or_default();
        let _ = fs::remove_file(path);
        text
    }

    fn state(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.path("state").join(name)).ok()
    }

    fn sys(&self, name: &str) -> String {
        self.path("systems").join(name).display().to_string()
    }

    /// The boot bits of systems/NAME, as boot.sh's own shell hashed them.
    fn bits(&self, name: &str) -> String {
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

fn chown(path: &Path) {
    std::os::unix::fs::lchown(path, Some(65534), Some(65534)).unwrap();
    if path.is_dir() && !path.is_symlink() {
        for entry in fs::read_dir(path).unwrap() {
            chown(&entry.unwrap().path());
        }
    }
}

fn said(out: &Output) -> (Option<i32>, String, String) {
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn ok(out: &str) -> (Option<i32>, String, String) {
    (Some(0), out.into(), String::new())
}

const USAGE: &str = "usage: box-boot default SYSTEM | trial [--force] | check | status\n";

#[test]
fn a_usage_or_a_config_it_cant_take_exits_2_having_run_nothing() {
    let m = Machine::new();
    let out = Command::new(BIN).output().unwrap();
    assert_eq!(
        said(&out),
        (
            Some(2),
            String::new(),
            "usage: burrow-boot CONFIG WORD\n".into()
        )
    );
    let out = Command::new(BIN)
        .arg(m.path("none.json"))
        .arg("status")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(said(&out).2.starts_with("burrow-boot: "));
    for words in [
        &["go"][..],
        &["default"],
        &["default", "a", "b"],
        &["trial", "now"],
        &["check", "--force"],
        &["__through-sudo", "go"],
    ] {
        assert_eq!(
            said(&m.run(words)),
            (Some(2), String::new(), USAGE.into()),
            "{words:?}"
        );
    }
    assert_eq!(m.fake("sudo"), "");
    assert!(!m.path("state").exists());
}

#[test]
fn status_says_what_booted_runs_and_boots_next_through_sudo() {
    let m = Machine::new();
    m.system("two", "k2", "i1", "panic=10 loglevel=4\n");
    fs::remove_file(m.path("run/current-system")).unwrap();
    symlink(m.path("systems/two"), m.path("run/current-system")).unwrap();
    let out = m.run(&[]);
    let one = m.sys("one");
    assert_eq!(
        said(&out),
        ok(&format!(
            "booted:       {one}\nrunning:      {}\nboot default: {one}\ntrial queued: none\n",
            m.sys("two")
        ))
    );
    let sudo = m.fake("sudo");
    assert!(sudo.starts_with("-- /"), "{sudo}");
    assert!(
        sudo.ends_with(&format!(
            " {} __through-sudo\n",
            m.path("config.json").display()
        )),
        "{sudo}"
    );
    // Its state dir, with both lists in it, and nothing else run.
    assert_eq!(m.state("boot-proven").as_deref(), Some(""));
    assert_eq!(m.state("boot-failed").as_deref(), Some(""));
    assert_eq!(m.fake("switch-to-configuration"), "");
    // The same said as `status`, and with a trial queued.
    m.run(&["default", &m.sys("two")]);
    let out = m.run(&["status"]);
    assert!(
        said(&out)
            .1
            .ends_with(&format!("trial queued: {}\n", m.sys("two")))
    );
}

#[test]
fn a_system_whose_bits_came_up_here_is_the_boot_default_at_once() {
    let m = Machine::new();
    // A new generation of what booted: the same kernel, initrd and params.
    m.system("one-again", "k1", "i1", "panic=10 loglevel=4\n\n");
    // Through a symlink to it, as a profile would be.
    symlink(m.path("systems/one-again"), m.path("profile")).unwrap();
    fs::create_dir(m.path("state")).unwrap();
    fs::write(m.path("state/boot-trial"), m.sys("one")).unwrap();
    let out = m.run(&["default", &m.path("profile").display().to_string()]);
    assert_eq!(
        said(&out),
        ok(&format!("boot default: {}\n", m.sys("one-again")))
    );
    assert_eq!(m.fake("switch-to-configuration"), "one-again boot\n");
    assert_eq!(
        m.state("boot-default"),
        Some(format!("{}\n", m.sys("one-again")))
    );
    // Its trial isn't one any more.
    assert_eq!(m.state("boot-trial"), None);
    // One whose bits came up here before, from the proven list.
    m.system("old", "k0", "i0", "quiet");
    fs::write(
        m.path("state/boot-proven"),
        format!("x\n{}\n", m.bits("old")),
    )
    .unwrap();
    let out = m.run(&["default", &m.sys("old")]);
    assert_eq!(said(&out), ok(&format!("boot default: {}\n", m.sys("old"))));
    assert_eq!(m.fake("switch-to-configuration"), "old boot\n");
    // A line is the bits only when it's them and nothing else.
    m.system("near", "k8", "i8", "quiet");
    let bits = m.bits("near").trim_end().to_string();
    fs::write(
        m.path("state/boot-proven"),
        format!("{bits}0\n {bits}\n{}\n", &bits[..63]),
    )
    .unwrap();
    let out = m.run(&["default", &m.sys("near")]);
    assert!(said(&out).1.contains("boots once in the next trial"));
}

#[test]
fn a_new_kernel_keeps_the_default_and_is_queued_for_a_trial() {
    let m = Machine::new();
    m.system("two", "k2", "i1", "panic=10 loglevel=4\n");
    let out = m.run(&["default", &m.sys("two")]);
    let (one, two) = (m.sys("one"), m.sys("two"));
    assert_eq!(
        said(&out),
        ok(&format!(
            "boot default stays {one}; {two} boots once in the next trial (box-boot trial)\n"
        ))
    );
    // The menu made again with what booted still its default.
    assert_eq!(m.fake("switch-to-configuration"), "one boot\n");
    assert_eq!(m.state("boot-default"), Some(format!("{one}\n")));
    assert_eq!(m.state("boot-trial"), Some(format!("{two}\n")));
    // A new initrd, and new params, are new bits too.
    for (name, kernel, initrd, params) in [
        ("initrd", "k1", "i2", "panic=10 loglevel=4"),
        ("params", "k1", "i1", "panic=10 loglevel=7"),
    ] {
        m.system(name, kernel, initrd, params);
        let out = m.run(&["default", &m.sys(name)]);
        assert!(
            said(&out).1.contains("boots once in the next trial"),
            "{name}"
        );
        assert_eq!(m.state("boot-trial"), Some(format!("{}\n", m.sys(name))));
    }
}

#[test]
fn the_default_it_made_stays_while_its_bits_came_up_here() {
    let m = Machine::new();
    m.system("good", "k0", "i0", "quiet");
    m.system("new", "k2", "i2", "quiet");
    fs::create_dir(m.path("state")).unwrap();
    fs::write(m.path("state/boot-default"), format!("{}\n", m.sys("good"))).unwrap();
    fs::write(m.path("state/boot-proven"), format!("{}\n", m.bits("good"))).unwrap();
    let out = m.run(&["default", &m.sys("new")]);
    assert!(
        said(&out)
            .1
            .starts_with(&format!("boot default stays {};", m.sys("good")))
    );
    assert_eq!(m.fake("switch-to-configuration"), "good boot\n");
    // Not proven, gone, or no system at all: what booted.
    for default in [
        m.sys("new"),
        m.sys("gone"),
        m.path("kernels").display().to_string(),
        String::new(),
    ] {
        fs::write(m.path("state/boot-default"), format!("{default}\n")).unwrap();
        let out = m.run(&["status"]);
        assert!(
            said(&out)
                .1
                .contains(&format!("boot default: {}\n", m.sys("one"))),
            "{default}: {}",
            said(&out).1
        );
    }
    // Proven, but not a system that can be made the default any more.
    fs::write(m.path("state/boot-default"), format!("{}\n", m.sys("good"))).unwrap();
    fs::remove_file(m.path("systems/good/bin/switch-to-configuration")).unwrap();
    let out = m.run(&["status"]);
    assert!(
        said(&out)
            .1
            .contains(&format!("boot default: {}\n", m.sys("one")))
    );
}

#[test]
fn a_kernel_that_failed_before_is_never_queued_again() {
    let m = Machine::new();
    m.system("bad", "k9", "i1", "quiet");
    fs::create_dir(m.path("state")).unwrap();
    fs::write(m.path("state/boot-failed"), format!("{}\n", m.bits("bad"))).unwrap();
    fs::write(m.path("state/boot-trial"), format!("{}\n", m.sys("one"))).unwrap();
    let out = m.run(&["default", &m.sys("bad")]);
    assert_eq!(
        said(&out),
        (
            Some(0),
            String::new(),
            format!(
                "boot default stays {}: {}'s kernel failed to boot before\n",
                m.sys("one"),
                m.sys("bad")
            )
        )
    );
    assert_eq!(m.fake("switch-to-configuration"), "one boot\n");
    // A trial already queued stays queued.
    assert_eq!(m.state("boot-trial"), Some(format!("{}\n", m.sys("one"))));
}

#[test]
fn a_default_that_isnt_a_system_or_wont_take_changes_nothing() {
    let m = Machine::new();
    let kernels = m.path("kernels").display().to_string();
    let out = m.run(&["default", &kernels]);
    assert_eq!(
        said(&out),
        (
            Some(1),
            String::new(),
            format!("box-boot: {kernels}/kernel: No such file or directory (os error 2)\n")
        )
    );
    let out = m.run(&["default", &m.sys("gone")]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(m.fake("switch-to-configuration"), "");
    assert_eq!(m.state("boot-default"), None);
    // Its switch-to-configuration failing: no default written, no trial.
    m.system("two", "k2", "i1", "quiet");
    m.set("fail-one", "");
    let out = m.run(&["default", &m.sys("two")]);
    assert_eq!(
        said(&out),
        (
            Some(1),
            String::new(),
            format!(
                "box-boot: {}/bin/switch-to-configuration boot: exit status: 1\n",
                m.sys("one")
            )
        )
    );
    assert_eq!(m.state("boot-default"), None);
    assert_eq!(m.state("boot-trial"), None);
}

/// A machine with `two`, a new kernel, queued for a trial a moment ago, and
/// in GRUB's menu.
fn queued() -> Machine {
    let m = Machine::new();
    m.system("two", "k2", "i1", "quiet");
    m.menu(&["one", "two"]);
    m.run(&["default", &m.sys("two")]);
    m.fake("switch-to-configuration");
    m.fake("sudo");
    m
}

#[test]
fn a_trial_reboots_once_into_its_menu_entry() {
    let m = queued();
    let out = m.run(&["trial"]);
    assert_eq!(said(&out), ok(""));
    assert_eq!(m.state("boot-trying"), Some(format!("{}\n", m.sys("two"))));
    let title = "NixOS - All configurations>NixOS - two";
    let (boot, fallback) = (
        m.path("boot/grub/grubenv"),
        m.path("boot-fallback/grub/grubenv"),
    );
    // Each grubenv made first, and none where there's no GRUB.
    assert_eq!(
        m.fake("grub-editenv"),
        format!(
            "{b} create\n{b} set next_entry={title}\n{f} create\n{f} set next_entry={title}\n",
            b = boot.display(),
            f = fallback.display()
        )
    );
    assert_eq!(
        m.fake("journal"),
        format!("-t box-boot -p warning rebooting once into: {title}\n")
    );
    let systemctl = m.fake("systemctl");
    assert!(systemctl.ends_with("\nreboot\n"), "{systemctl}");
    // The box was idle: it asked.
    assert!(systemctl.starts_with("--user -M sam@ list-units --plain --no-legend --state=active proj-*.service\nlist-units --plain --no-legend --state=active microvm@*.service\n"), "{systemctl}");
    // The trial stays queued until the boot after settles it, and a grubenv
    // there already is used as it is.
    assert!(m.state("boot-trial").is_some());
    m.run(&["trial"]);
    assert_eq!(
        m.fake("grub-editenv"),
        format!(
            "{b} set next_entry={title}\n{f} set next_entry={title}\n",
            b = boot.display(),
            f = fallback.display()
        )
    );
}

#[test]
fn nothing_queued_or_proven_since_is_no_reboot() {
    let m = Machine::new();
    assert_eq!(said(&m.run(&["trial", "--force"])), ok("no trial queued\n"));
    let m = queued();
    // A trial of what came up since (the kernel booted by hand, say).
    fs::write(m.path("state/boot-proven"), format!("{}\n", m.bits("two"))).unwrap();
    let out = m.run(&["trial"]);
    assert_eq!(
        said(&out),
        ok(&format!(
            "already proven; {} is the boot default\n",
            m.sys("two")
        ))
    );
    assert_eq!(m.fake("switch-to-configuration"), "two boot\n");
    assert_eq!(m.state("boot-default"), Some(format!("{}\n", m.sys("two"))));
    assert_eq!(m.state("boot-trial"), None);
    assert_eq!(m.fake("systemctl"), "");
}

#[test]
fn jobs_or_sandboxes_put_a_trial_off_for_two_days_unless_forced() {
    let postponed = ok("jobs or sandboxes running; trial postponed\n");
    for busy in ["jobs", "sandboxes"] {
        let m = queued();
        m.set(busy, "proj-1.service loaded active running x\n");
        assert_eq!(said(&m.run(&["trial"])), postponed, "{busy}");
        assert!(!m.fake("systemctl").contains("reboot"));
        assert_eq!(m.state("boot-trying"), None);
        // Forced, it goes.
        assert_eq!(said(&m.run(&["trial", "--force"])), ok(""), "{busy}");
        assert!(m.fake("systemctl").ends_with("reboot\n"));
    }
    // Queued two days ago, it goes whatever's running.
    let m = queued();
    m.set("jobs", "proj-1.service loaded active running x\n");
    let trial = File::options()
        .write(true)
        .open(m.path("state/boot-trial"))
        .unwrap();
    let ago = |secs| SystemTime::now() - Duration::from_secs(secs);
    trial.set_modified(ago(2 * 24 * 3600 - 60)).unwrap();
    assert_eq!(said(&m.run(&["trial"])), postponed);
    trial.set_modified(ago(2 * 24 * 3600 + 1)).unwrap();
    assert_eq!(said(&m.run(&["trial"])), ok(""));
    assert!(m.fake("systemctl").ends_with("reboot\n"));
}

#[test]
fn a_pending_switch_puts_a_trial_off_even_forced() {
    let m = queued();
    m.set("jobs", "");
    fs::write(m.path("switch/pending"), "").unwrap();
    for words in [&["trial"][..], &["trial", "--force"]] {
        assert_eq!(
            said(&m.run(words)),
            ok("a switch is pending; trial postponed\n")
        );
    }
    assert!(!m.fake("systemctl").contains("reboot"));
    assert_eq!(m.fake("grub-editenv"), "");
}

#[test]
fn a_trial_not_in_the_menu_or_that_cant_be_set_reboots_nothing() {
    let m = queued();
    m.menu(&["one"]);
    let out = m.run(&["trial"]);
    assert_eq!(
        said(&out),
        (
            Some(1),
            String::new(),
            format!("box-boot: {} is not in the GRUB menu\n", m.sys("two"))
        )
    );
    assert_eq!(m.state("boot-trying"), None);
    assert!(!m.fake("systemctl").contains("reboot"));
    // grubenv refusing it: the boot after this one isn't a trial.
    m.menu(&["one", "two"]);
    m.set("fail-editenv-set", "");
    let out = m.run(&["trial"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        said(&out)
            .2
            .contains("set next_entry=NixOS - All configurations>NixOS - two: exit status: 1")
    );
    assert_eq!(m.state("boot-trying"), None);
    let editenv = m.fake("grub-editenv");
    for grub in ["boot", "boot-fallback"] {
        let env = m.path(grub).join("grub/grubenv");
        assert!(
            editenv.contains(&format!("{} unset next_entry\n", env.display())),
            "{editenv}"
        );
    }
    assert!(!m.fake("systemctl").contains("reboot"));
    // A reboot that fails says so.
    fs::remove_file(m.path("fake/fail-editenv-set")).unwrap();
    m.set("fail-reboot", "");
    let out = m.run(&["trial"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).2.ends_with("systemctl reboot: exit status: 1\n"));
}

#[test]
fn a_trial_a_grubenv_wont_let_go_of_is_still_checked_as_one() {
    let m = queued();
    let (boot, fallback) = (
        m.path("boot/grub/grubenv"),
        m.path("boot-fallback/grub/grubenv"),
    );
    let unset = |env: &Path| format!("{} unset next_entry\n", env.display());
    // Set in /boot, refused in /boot-fallback: taken back from both.
    m.set("fail-editenv-set-boot-fallback", "");
    let out = m.run(&["trial"]);
    let (code, _, stderr) = said(&out);
    assert_eq!(code, Some(1));
    assert!(
        stderr.contains(&format!("{} set next_entry=", fallback.display())),
        "{stderr}"
    );
    let editenv = m.fake("grub-editenv");
    assert!(editenv.contains(&unset(&boot)), "{editenv}");
    assert!(editenv.contains(&unset(&fallback)), "{editenv}");
    assert_eq!(m.state("boot-trying"), None);
    // /boot won't let it go either. The take-back still reaches
    // /boot-fallback, and as the boot after may be the trial, it's still
    // tracked as one.
    m.set("fail-editenv-unset-boot", "");
    let out = m.run(&["trial"]);
    let (code, _, stderr) = said(&out);
    assert_eq!(code, Some(1));
    // What it couldn't take back, after what it couldn't set.
    let back = stderr.split_once("; taking it back: ").map(|(_, b)| b);
    assert!(
        back.is_some_and(|b| b.ends_with(&format!(
            "{} unset next_entry: exit status: 1\n",
            boot.display()
        ))),
        "{stderr}"
    );
    assert!(m.fake("grub-editenv").contains(&unset(&fallback)));
    assert_eq!(m.state("boot-trying"), Some(format!("{}\n", m.sys("two"))));
    assert!(!m.fake("systemctl").contains("reboot"));
    // GRUB boots it, without sshd: checked as the trial it is.
    fs::remove_file(m.path("fake/fail-editenv-unset-boot")).unwrap();
    m.boots("two");
    m.set("sshd-down", "");
    assert_eq!(said(&m.run(&["check"])), ok(""));
    assert_eq!(m.state("boot-failed"), Some(m.bits("two")));
    assert!(
        m.fake("systemctl")
            .ends_with("-q is-active sshd.service\nreboot\n")
    );
    assert_eq!(m.state("boot-trying"), None);
}

#[test]
fn a_notice_that_wont_open_or_close_fails_check_and_settles_it_all_the_same() {
    // A trial that didn't come up, one without sshd, and one that passed.
    for (came_up, sshd, word) in [
        (false, true, "open"),
        (true, false, "open"),
        (true, true, "close"),
    ] {
        let m = queued();
        m.run(&["trial"]);
        m.fake("systemctl");
        if came_up {
            m.boots("two");
        }
        if !sshd {
            m.set("sshd-down", "");
        }
        m.set("fail-notify", "");
        let out = m.run(&["check"]);
        let (code, stdout, stderr) = said(&out);
        let case = format!("{word}, came up {came_up}, sshd {sshd}");
        assert_eq!((code, stdout.as_str()), (Some(1), ""), "{case}");
        assert!(stderr.starts_with("box-boot: "), "{case}: {stderr}");
        assert!(
            stderr.ends_with(&format!("box-notify {word}: exit status: 1\n")),
            "{case}: {stderr}"
        );
        assert_eq!(m.fake("notify").split('|').next(), Some(word), "{case}");
        assert_eq!(m.state("boot-trial"), None, "{case}");
        assert_eq!(m.state("boot-trying"), None, "{case}");
        let systemctl = m.fake("systemctl");
        if came_up && sshd {
            assert_eq!(m.state("boot-proven"), Some(m.bits("two")), "{case}");
            assert_eq!(m.state("boot-default"), Some(format!("{}\n", m.sys("two"))));
            assert!(!systemctl.contains("reboot"), "{case}");
        } else {
            assert_eq!(m.state("boot-failed"), Some(m.bits("two")), "{case}");
            assert_eq!(
                systemctl.ends_with("reboot\n"),
                came_up,
                "{case}: {systemctl}"
            );
        }
    }
}

#[test]
fn after_a_boot_its_bits_are_proven_once_and_next_entry_cleared() {
    let m = Machine::new();
    assert_eq!(said(&m.run(&["check"])), ok(""));
    // As boot.sh hashed them, so the lists it kept still hold.
    assert_eq!(m.state("boot-proven"), Some(m.bits("one")));
    let editenv = m.fake("grub-editenv");
    assert!(
        editenv.ends_with(&format!(
            "{} unset next_entry\n",
            m.path("boot-fallback/grub/grubenv").display()
        )),
        "{editenv}"
    );
    assert_eq!(said(&m.run(&["check"])), ok(""));
    assert_eq!(m.state("boot-proven"), Some(m.bits("one")));
    // No trial: nothing else.
    assert_eq!(m.fake("switch-to-configuration"), "");
    assert_eq!(m.fake("notify"), "");
    assert_eq!(m.fake("journal"), "");
    assert!(!m.fake("systemctl").contains("reboot"));
}

#[test]
fn the_bits_are_its_kernel_initrd_and_params_hashed_as_boot_sh_did() {
    let m = Machine::new();
    for (name, params) in [
        ("plain", "quiet"),
        ("newlines", "a=1\n\n\n"),
        ("inside", "a=1\nb=2\n"),
        ("none", ""),
        ("spaces", "  a  b  "),
    ] {
        m.system(name, "k1", "i1", params);
        m.boots(name);
        let _ = fs::write(m.path("state/boot-proven"), "");
        m.run(&["check"]);
        assert_eq!(m.state("boot-proven"), Some(m.bits(name)), "{name}");
    }
}

#[test]
fn a_trial_that_came_up_is_the_boot_default() {
    let m = queued();
    m.run(&["trial"]);
    m.fake("journal");
    m.boots("two");
    let out = m.run(&["check"]);
    assert_eq!(said(&out), ok(""));
    assert_eq!(m.state("boot-proven"), Some(m.bits("two")));
    assert_eq!(m.fake("switch-to-configuration"), "two boot\n");
    assert_eq!(m.state("boot-default"), Some(format!("{}\n", m.sys("two"))));
    assert_eq!(m.state("boot-trial"), None);
    assert_eq!(m.state("boot-trying"), None);
    assert_eq!(
        m.fake("journal"),
        format!(
            "-t box-boot -p notice trial passed: {} is the boot default\n",
            m.sys("two")
        )
    );
    assert_eq!(
        m.fake("notify"),
        format!(
            "close|boot-trial|The trial boot of {} worked.\n",
            m.sys("two")
        )
    );
    let systemctl = m.fake("systemctl");
    assert!(
        systemctl.ends_with("-q is-active sshd.service\n"),
        "{systemctl}"
    );
}

#[test]
fn a_trial_that_didnt_come_up_is_never_tried_again() {
    let m = queued();
    m.run(&["trial"]);
    m.fake("journal");
    m.fake("systemctl");
    // The watchdog reset it, and GRUB booted the default.
    let out = m.run(&["check"]);
    assert_eq!(said(&out), ok(""));
    let (one, two) = (m.sys("one"), m.sys("two"));
    assert_eq!(m.state("boot-failed"), Some(m.bits("two")));
    assert_eq!(m.state("boot-proven"), Some(String::new()));
    assert_eq!(m.state("boot-trial"), None);
    assert_eq!(m.state("boot-trying"), None);
    assert_eq!(
        m.fake("journal"),
        format!("-t box-boot -p err TRIAL FAILED: {two} did not come up; booted {one} instead\n")
    );
    assert_eq!(
        m.fake("notify"),
        format!(
            "open|boot-trial|A new kernel didn't boot|box tried to boot {two} once and came back up on {one} instead (panic, hang or watchdog reset). That kernel won't be tried again. The default is still the last good one. Claude is looking into it.\n"
        )
    );
    assert_eq!(m.fake("systemctl"), "");
    assert_eq!(m.fake("switch-to-configuration"), "");
    // Never queued again.
    let out = m.run(&["default", &two]);
    assert!(said(&out).2.ends_with("'s kernel failed to boot before\n"));
    assert_eq!(m.state("boot-trial"), None);
}

#[test]
fn a_trial_that_came_up_without_sshd_reboots_into_the_default() {
    let m = queued();
    m.run(&["trial"]);
    m.fake("journal");
    m.fake("systemctl");
    m.boots("two");
    m.set("sshd-down", "");
    let out = m.run(&["check"]);
    assert_eq!(said(&out), ok(""));
    let two = m.sys("two");
    assert_eq!(m.state("boot-failed"), Some(m.bits("two")));
    assert_eq!(m.state("boot-proven"), Some(String::new()));
    assert_eq!(m.state("boot-trial"), None);
    assert_eq!(
        m.fake("journal"),
        format!(
            "-t box-boot -p err TRIAL FAILED: {two} booted without sshd; rebooting into the default\n"
        )
    );
    assert_eq!(
        m.fake("notify"),
        format!(
            "open|boot-trial|A new kernel booted broken|box booted {two} once, but sshd didn't come up, so it rebooted into the last good system. That kernel won't be tried again.\n"
        )
    );
    assert_eq!(m.fake("systemctl"), "-q is-active sshd.service\nreboot\n");
    assert_eq!(m.fake("switch-to-configuration"), "");
}

#[test]
fn a_failed_trial_reboots_whatever_else_breaks_and_says_it() {
    let m = queued();
    m.run(&["trial"]);
    m.fake("grub-editenv");
    m.boots("two");
    m.set("sshd-down", "");
    m.set("fail-notify", "");
    m.set("fail-editenv-unset", "");
    // Gone since: its bits can't be had.
    fs::remove_file(m.path("systems/two/kernel-params")).unwrap();
    let out = m.run(&["check"]);
    let (code, stdout, stderr) = said(&out);
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    assert!(
        stderr.contains("unset next_entry: exit status: 1\n"),
        "{stderr}"
    );
    assert!(
        stderr.contains("kernel-params: No such file or directory"),
        "{stderr}"
    );
    assert!(
        stderr.contains("box-notify open: exit status: 1\n"),
        "{stderr}"
    );
    assert!(m.fake("systemctl").ends_with("reboot\n"));
    assert_eq!(m.state("boot-trial"), None);
    assert_eq!(m.state("boot-trying"), None);
}
