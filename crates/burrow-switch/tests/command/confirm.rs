use std::fs;

use burrow_test::said;

use crate::machine::{CONFIRM, Machine, ok, refused};

#[test]
fn a_usage_or_nothing_pending_does_nothing() {
    let m = Machine::new(false);
    let out = m.command(CONFIRM, &["now"]).output().unwrap();
    assert_eq!(said(&out), refused(2, "usage: box-confirm\n"));
    assert_eq!(m.fake("sudo"), "");
    assert_eq!(said(&m.confirm()), ok("nothing pending\n"));
    // Through sudo.
    assert!(m.fake("sudo").ends_with(" __through-sudo\n"));
    assert_eq!(m.fake("nix-env"), "");
}

#[test]
fn what_it_cant_read_it_says_and_exits_1() {
    let m = Machine::switched(false);
    fs::remove_file(m.path("state/pending")).unwrap();
    fs::create_dir(m.path("state/pending")).unwrap();
    let out = m.confirm();
    let (code, stdout, stderr) = said(&out);
    assert_eq!((code, stdout.as_str()), (Some(1), ""));
    let pending = m.path("state/pending");
    assert!(
        stderr.starts_with(&format!("box-confirm: {}: ", pending.display())),
        "{stderr}"
    );
    assert_eq!(m.fake("nix-env"), "");
}

#[test]
fn a_confirm_makes_it_the_boot_default_then_disarms_the_revert() {
    let m = Machine::switched(false);
    let two = m.sys("two");
    assert_eq!(said(&m.confirm()), ok(&format!("confirmed: {two}\n")));
    assert_eq!(
        m.fake("nix-env"),
        format!(
            "NIX_REMOTE=daemon -p {} --set {two}\n",
            m.path("profile").display()
        )
    );
    assert_eq!(m.profile(), two);
    // Without trial boots, its own switch-to-configuration.
    assert_eq!(m.fake("switch-to-configuration"), "two boot\n");
    assert_eq!(
        m.fake("systemctl"),
        "show -p ActiveState --value box-revert.service\nstop box-revert.timer\nshow -p ActiveState --value box-revert.service\n"
    );
    assert_eq!(m.state("pending"), None);
    assert_eq!(m.state("prev"), None);
    assert_eq!(said(&m.confirm()), ok("nothing pending\n"));
}

#[test]
fn with_trial_boots_host_boot_makes_it_the_default() {
    let m = Machine::switched(true);
    let two = m.sys("two");
    assert_eq!(said(&m.confirm()).0, Some(0));
    assert_eq!(m.fake("box-boot"), format!("default {two}\n"));
    assert_eq!(m.fake("switch-to-configuration"), "");
}

#[test]
fn a_confirm_too_late_or_of_what_doesnt_run_keeps_nothing() {
    let m = Machine::switched(false);
    // Running as it's looked at (and only then, so a confirm that didn't
    // look would go on, not wait for ever).
    m.set("activating-for", "1");
    assert_eq!(
        said(&m.confirm()),
        refused(1, "too late: the revert is already running\n")
    );
    let m = Machine::switched(false);
    m.runs("one");
    assert_eq!(
        said(&m.confirm()),
        refused(
            1,
            "running system isn't the pending one (did it revert?); not confirming\n"
        )
    );
    assert_eq!(m.fake("nix-env"), "");
    assert_eq!(m.profile(), m.sys("one"));
}

#[test]
fn a_boot_default_it_cant_make_leaves_the_revert_armed() {
    for fail in ["fail-nix-env", "fail-boot"] {
        let m = Machine::switched(true);
        m.set(fail, "");
        let out = m.confirm();
        let (code, stdout, stderr) = said(&out);
        assert_eq!((code, stdout.as_str()), (Some(1), ""), "{fail}");
        assert!(
            stderr.ends_with(
                ": exit status: 1\ncouldn't make it the boot default; still pending, and it reverts on schedule\n"
            ),
            "{fail}: {stderr}"
        );
        assert!(stderr.starts_with("box-confirm: "), "{fail}: {stderr}");
        assert!(!m.fake("systemctl").contains("stop"), "{fail}");
        assert_eq!(m.state("pending"), Some(format!("{}\n", m.sys("two"))));
    }
}

#[test]
fn a_revert_that_lands_while_confirming_boots_what_runs() {
    let m = Machine::switched(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    // Its timer fires just before it's stopped, and the revert lands while
    // it's waited for.
    m.set("fires-on-stop", "1");
    m.set(
        "lands",
        &format!("ln -sfn {one} {}\n", m.path("run/current-system").display()),
    );
    assert_eq!(
        said(&m.confirm()),
        refused(
            1,
            &format!("too late: it reverted while confirming; {one} is running\n")
        )
    );
    let profile = m.path("profile").display().to_string();
    assert_eq!(
        m.fake("nix-env"),
        format!(
            "NIX_REMOTE=daemon -p {profile} --set {two}\nNIX_REMOTE=daemon -p {profile} --set {one}\n"
        )
    );
    assert_eq!(m.fake("switch-to-configuration"), "two boot\none boot\n");
    assert_eq!(m.profile(), one);
    assert_eq!(m.state("pending"), None);
    assert_eq!(m.state("prev"), None);
}
