use std::fs;
use std::os::unix::fs::symlink;
use std::process::Command;

use crate::machine::{BIN, Machine, USAGE, ok, said};

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
