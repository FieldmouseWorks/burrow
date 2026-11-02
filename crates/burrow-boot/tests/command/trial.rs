use std::fs::{self, File};
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::machine::{Machine, ok, queued, said};

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
fn a_later_trial_waits_while_a_grubenv_may_boot_an_older_one() {
    let m = queued();
    let (boot, fallback) = (
        m.path("boot/grub/grubenv"),
        m.path("boot-fallback/grub/grubenv"),
    );
    m.run(&["trial"]);
    // Two didn't come up, and /boot-fallback won't let it go.
    m.set("fail-editenv-unset-boot-fallback", "");
    m.run(&["check"]);
    assert_eq!(m.state("boot-failed"), Some(m.bits("two")));
    // Three queued since, which /boot-fallback won't take either.
    m.system("three", "k3", "i1", "quiet");
    m.menu(&["one", "two", "three"]);
    let (two, three) = (m.sys("two"), m.sys("three"));
    m.run(&["default", &three]);
    m.set("fail-editenv-set-boot-fallback", "");
    for f in ["grub-editenv", "systemctl"] {
        m.fake(f);
    }
    // However often it's tried, forced or not, three waits, and two is the
    // trial still tracked.
    for words in [&["trial"][..], &["trial", "--force"], &["trial"]] {
        let (code, stdout, stderr) = said(&m.run(words));
        assert_eq!((code, stdout.as_str()), (Some(1), ""), "{words:?}");
        assert!(
            stderr.starts_with(&format!(
                "box-boot: a grubenv may still boot the trial of {two}, so {three} waits: "
            )),
            "{stderr}"
        );
        assert!(
            stderr.ends_with(&format!(
                "{} unset next_entry: exit status: 1\n",
                fallback.display()
            )),
            "{stderr}"
        );
        assert_eq!(m.state("boot-trying"), Some(format!("{two}\n")));
        let editenv = m.fake("grub-editenv");
        assert!(!editenv.contains(" set "), "{editenv}");
        assert!(!m.fake("systemctl").contains("reboot"));
    }
    assert_eq!(m.state("boot-trial"), Some(format!("{three}\n")));
    // Booted from /boot-fallback's disk, two is rebooted out of as the
    // failed trial it is, never proven; and three isn't taken for failed.
    m.boots("two");
    assert_eq!(said(&m.run(&["check"])).0, Some(1));
    assert!(m.fake("systemctl").ends_with("reboot\n"));
    assert!(!m.state("boot-proven").unwrap().contains(&m.bits("two")));
    assert_eq!(m.state("boot-failed"), Some(m.bits("two")));
    assert_eq!(m.state("boot-trial"), Some(format!("{three}\n")));
    // Once /boot-fallback lets go, three's trial takes its place.
    for f in [
        "fail-editenv-unset-boot-fallback",
        "fail-editenv-set-boot-fallback",
    ] {
        fs::remove_file(m.path("fake").join(f)).unwrap();
    }
    m.boots("one");
    m.fake("grub-editenv");
    assert_eq!(said(&m.run(&["trial"])), ok(""));
    let title = "NixOS - All configurations>NixOS - three";
    assert_eq!(
        m.fake("grub-editenv"),
        format!(
            "{b} unset next_entry\n{f} unset next_entry\n{b} set next_entry={title}\n{f} set next_entry={title}\n",
            b = boot.display(),
            f = fallback.display()
        )
    );
    assert_eq!(m.state("boot-trying"), Some(format!("{three}\n")));
    assert!(m.fake("systemctl").ends_with("reboot\n"));
}

#[test]
fn an_older_trial_that_never_booted_makes_way_once_let_go_of() {
    let m = queued();
    // Two's trial couldn't be set in /boot-fallback, nor taken back from
    // /boot: no reboot, and still tracked.
    m.set("fail-editenv-set-boot-fallback", "");
    m.set("fail-editenv-unset-boot", "");
    assert_eq!(said(&m.run(&["trial"])).0, Some(1));
    assert_eq!(m.state("boot-trying"), Some(format!("{}\n", m.sys("two"))));
    // Three queued since: while /boot keeps two, it waits.
    m.system("three", "k3", "i1", "quiet");
    m.menu(&["one", "two", "three"]);
    m.run(&["default", &m.sys("three")]);
    assert_eq!(said(&m.run(&["trial"])).0, Some(1));
    assert_eq!(m.state("boot-trying"), Some(format!("{}\n", m.sys("two"))));
    // Once /boot lets go, three goes, and two is neither proven nor failed.
    for f in ["fail-editenv-set-boot-fallback", "fail-editenv-unset-boot"] {
        fs::remove_file(m.path("fake").join(f)).unwrap();
    }
    m.fake("systemctl");
    assert_eq!(said(&m.run(&["trial"])), ok(""));
    assert_eq!(
        m.state("boot-trying"),
        Some(format!("{}\n", m.sys("three")))
    );
    assert!(m.fake("systemctl").ends_with("reboot\n"));
    assert_eq!(m.state("boot-failed"), Some(String::new()));
    assert_eq!(m.state("boot-proven"), Some(String::new()));
}
