//! A confirm or an abort told the system it means (as HOST-update tells
//! them its own): another pending in its place is someone's switch,
//! mid-change, and is left be.

use std::os::unix::fs::symlink;

use crate::machine::{CONFIRM, Machine, ok, refused, said};

/// Nothing a refusal may touch was run: systemctl only asked.
fn untouched(m: &Machine) {
    let systemctl = m.fake("systemctl");
    assert!(
        systemctl.lines().all(|l| l.starts_with("show ")),
        "{systemctl}"
    );
    for f in [
        "systemd-run",
        "switch-to-configuration",
        "nix-env",
        "journal",
    ] {
        assert_eq!(m.fake(f), "", "{f}");
    }
}

#[test]
fn a_confirm_keeps_only_the_system_it_means() {
    // Another's switch replaced it: exit 4, and nothing changed.
    let m = Machine::switched(false);
    let (one, two, three) = (m.sys("one"), m.sys("two"), m.system("three", None));
    let out = m.command(CONFIRM, &[&three]).output().unwrap();
    assert_eq!(
        said(&out),
        refused(
            4,
            &format!("{three} isn't pending ({two} is); not confirming it\n")
        )
    );
    // It took its system through sudo with it.
    assert!(
        m.fake("sudo")
            .ends_with(&format!(" __through-sudo {three}\n"))
    );
    untouched(&m);
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(m.state("prev"), Some(format!("{one}\n")));
    assert_eq!(m.profile(), one);
    // Its own pending, named as a link to it, it confirms.
    let link = m.path("result");
    symlink(&two, &link).unwrap();
    let out = m
        .command(CONFIRM, &[link.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(said(&out), ok(&format!("confirmed: {two}\n")));
    assert_eq!((m.profile(), m.state("pending")), (two.clone(), None));
    for f in [
        "sudo",
        "systemctl",
        "nix-env",
        "switch-to-configuration",
        "journal",
    ] {
        m.fake(f);
    }
    // Nothing pending: it's kept already when it's the confirmed one, and
    // isn't when it isn't (reverted, or another's was confirmed).
    let out = m.command(CONFIRM, &[&two]).output().unwrap();
    assert_eq!(said(&out), ok(&format!("already confirmed: {two}\n")));
    let out = m.command(CONFIRM, &[&three]).output().unwrap();
    assert_eq!(
        said(&out),
        refused(
            4,
            &format!("{three} isn't pending (nothing is); not confirming it\n")
        )
    );
    m.fake("sudo");
    untouched(&m);
}

#[test]
fn an_abort_takes_back_only_the_system_it_means() {
    let m = Machine::switched(false);
    let (one, two, three) = (m.sys("one"), m.sys("two"), m.system("three", None));
    let out = m.switch(&["--abort", &three]);
    assert_eq!(
        said(&out),
        refused(
            4,
            &format!("{two} is pending, not {three}; leaving it be (it's someone's, mid-change)\n")
        )
    );
    untouched(&m);
    assert_eq!(m.running(), two);
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    // Its own it takes back, as any abort does.
    assert_eq!(said(&m.switch(&["--abort", &two])), ok(""));
    assert_eq!(m.running(), one);
    assert_eq!(m.state("pending"), None);
    // Nothing pending is nothing to take back, whichever it means.
    assert_eq!(
        said(&m.switch(&["--abort", &three])),
        ok("nothing pending\n")
    );
}
