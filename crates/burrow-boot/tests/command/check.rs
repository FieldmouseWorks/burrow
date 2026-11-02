use std::fs;

use crate::machine::{Machine, ok, queued, said};

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
    // Still tracked: no grubenv let go of it.
    assert_eq!(m.state("boot-trying"), Some(format!("{}\n", m.sys("two"))));
}

#[test]
fn a_failed_trial_a_grubenv_keeps_is_rebooted_out_of_whenever_it_boots() {
    let m = queued();
    let two = m.sys("two");
    m.run(&["trial"]);
    // It didn't come up, and /boot-fallback won't let it go.
    m.set("fail-editenv-unset-boot-fallback", "");
    for f in ["journal", "notify", "systemctl"] {
        m.fake(f);
    }
    let out = m.run(&["check"]);
    assert_eq!(said(&out).0, Some(1));
    assert_eq!(m.state("boot-failed"), Some(m.bits("two")));
    assert_eq!(m.state("boot-trial"), None);
    assert!(m.fake("notify").starts_with("open|"));
    assert_eq!(m.state("boot-trying"), Some(format!("{two}\n")));
    // An ordinary boot after: it's settled already, so nothing again.
    m.fake("journal");
    let out = m.run(&["check"]);
    assert_eq!(said(&out).0, Some(1));
    assert_eq!(m.fake("notify"), "");
    assert_eq!(m.fake("journal"), "");
    assert_eq!(m.state("boot-failed"), Some(m.bits("two")));
    assert!(!m.fake("systemctl").contains("reboot"));
    // Booted from /boot-fallback's disk: the failed trial, sshd and all.
    // Rebooted out of, and never proven.
    m.boots("two");
    let out = m.run(&["check"]);
    assert_eq!(said(&out).0, Some(1));
    assert_eq!(
        m.fake("journal"),
        format!(
            "-t box-boot -p err TRIAL FAILED: {two} booted again, from a grubenv that kept it; rebooting into the default\n"
        )
    );
    assert!(m.fake("systemctl").ends_with("reboot\n"));
    assert!(!m.state("boot-proven").unwrap().contains(&m.bits("two")));
    assert_eq!(m.fake("notify"), "");
    // Once it lets go, it's done.
    fs::remove_file(m.path("fake/fail-editenv-unset-boot-fallback")).unwrap();
    m.boots("one");
    assert_eq!(said(&m.run(&["check"])), ok(""));
    assert_eq!(m.state("boot-trying"), None);
}

#[test]
fn a_passed_trial_a_grubenv_keeps_is_settled_once() {
    let m = queued();
    let two = m.sys("two");
    m.run(&["trial"]);
    m.boots("two");
    m.set("fail-editenv-unset-boot-fallback", "");
    m.fake("notify");
    let out = m.run(&["check"]);
    assert_eq!(said(&out).0, Some(1));
    assert_eq!(m.state("boot-default"), Some(format!("{two}\n")));
    assert!(m.fake("notify").starts_with("close|"));
    assert_eq!(m.state("boot-trying"), Some(format!("{two}\n")));
    // A boot after: what's left of it boots a proven system.
    for f in ["journal", "switch-to-configuration", "systemctl"] {
        m.fake(f);
    }
    let out = m.run(&["check"]);
    assert_eq!(said(&out).0, Some(1));
    assert_eq!(m.fake("notify"), "");
    assert_eq!(m.fake("journal"), "");
    assert_eq!(m.fake("switch-to-configuration"), "");
    assert!(!m.fake("systemctl").contains("reboot"));
    fs::remove_file(m.path("fake/fail-editenv-unset-boot-fallback")).unwrap();
    assert_eq!(said(&m.run(&["check"])), ok(""));
    assert_eq!(m.state("boot-trying"), None);
    assert_eq!(m.state("boot-proven"), Some(m.bits("two")));
}
