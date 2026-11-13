use std::fs::{self, File};
use std::os::unix::fs::symlink;
use std::process::Stdio;
use std::thread;
use std::time::Duration;

use crate::machine::{CONFIRM, Machine, SWITCH, chown, ok, refused, root, said, unit_path};

const USAGE: &str =
    "usage: box-switch [FLAKE_DIR | --toplevel PATH | --status | --abort] [--allow-downgrade]\n";

#[test]
fn a_usage_or_a_config_it_cant_take_exits_2_having_run_nothing() {
    let m = Machine::new(false);
    let out = std::process::Command::new(SWITCH).output().unwrap();
    assert_eq!(
        said(&out),
        refused(2, "usage: burrow-switch CONFIG [WORDS]\n")
    );
    let out = std::process::Command::new(SWITCH)
        .arg(m.path("none.json"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(said(&out).2.starts_with("burrow-switch: "));
    for words in [
        &["--go"][..],
        &["--toplevel"],
        &["--status", "now"],
        &["a", "b"],
        &["__through-sudo"],
    ] {
        assert_eq!(said(&m.switch(words)), refused(2, USAGE), "{words:?}");
    }
    assert_eq!(m.fake("sudo"), "");
    // A window systemd can't read refuses before anything.
    let out = m
        .command(SWITCH, &["--toplevel", &m.sys("two")])
        .env("BURROW_CONFIRM_WITHIN", "soon")
        .output()
        .unwrap();
    assert_eq!(
        said(&out),
        refused(2, "box-switch: soon: not a time span systemd reads\n")
    );
    assert_eq!(m.fake("systemd-run"), "");
    assert!(!m.path("state").exists());
}

#[test]
fn a_switch_arms_the_revert_then_activates_with_a_deadline() {
    let m = Machine::new(false);
    let (one, two, me, path) = (m.sys("one"), m.sys("two"), m.me(), unit_path());
    let out = m.switch(&["--toplevel", &two]);
    assert_eq!(
        said(&out),
        ok(&format!(
            "live: {two}\nrun box-confirm within 10min to keep it (and make it the boot default)\n"
        ))
    );
    // Through sudo, with its window.
    let sudo = m.fake("sudo");
    assert!(
        sudo.ends_with(&format!(" __through-sudo 10min --toplevel {two}\n")),
        "{sudo}"
    );
    assert_eq!(
        m.fake("systemd-run"),
        format!(
            "--unit=box-revert --on-active=10min --timer-property=AccuracySec=1s --property=Type=oneshot --property=ManagedOOMPreference=omit --setenv=PATH={path} {me} --revert-to {one}\n\
             --unit=box-switch-activate --property=ManagedOOMPreference=omit --collect --wait --pipe --quiet --property=RuntimeMaxSec=450000000us --property=TimeoutStopSec=5s --property=ExecStopPost={me} --activation-stopped {two} {two}/bin/switch-to-configuration test\n"
        )
    );
    assert_eq!(m.fake("switch-to-configuration"), "two test\n");
    assert_eq!(m.running(), two);
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(m.state("prev"), Some(format!("{one}\n")));
    // An earlier switch disarmed first, and any revert of it waited out.
    let systemctl = m.fake("systemctl");
    assert!(
        systemctl.starts_with("stop box-revert.timer\nshow -p ActiveState --value box-revert.service\nreset-failed box-revert.timer box-revert.service\n"),
        "{systemctl}"
    );
    // Not the boot default: neither the profile nor the bootloader.
    assert_eq!(m.profile(), one);
    assert_eq!(m.fake("nix-env"), "");
    let out = m.switch(&["--status"]);
    assert_eq!(
        said(&out),
        ok(&format!(
            "pending: {two}\nreverts to: {one}\nMon 10:10 9min - - box-revert.timer box-revert.service\n"
        ))
    );
}

#[test]
fn its_window_comes_from_burrow_confirm_within_through_sudo() {
    let m = Machine::new(false);
    let two = m.sys("two");
    let out = m
        .command(SWITCH, &["--toplevel", &two])
        .env("BURROW_CONFIRM_WITHIN", "100s")
        .output()
        .unwrap();
    assert!(
        said(&out)
            .1
            .contains("run box-confirm within 100s to keep it")
    );
    assert!(m.fake("sudo").contains(" __through-sudo 100s --toplevel "));
    let run = m.fake("systemd-run");
    assert!(run.contains("--on-active=100s "), "{run}");
    assert!(
        run.contains("--property=RuntimeMaxSec=75000000us "),
        "{run}"
    );
    // Set but empty: the config's.
    let m = Machine::new(false);
    let out = m
        .command(SWITCH, &["--toplevel", &two])
        .env("BURROW_CONFIRM_WITHIN", "")
        .output()
        .unwrap();
    assert!(said(&out).1.contains(" within 10min "), "{:?}", said(&out));
}

#[test]
fn its_units_run_it_with_its_config_wherever_that_was_named() {
    let m = Machine::new(false);
    let (one, two, me) = (m.sys("one"), m.sys("two"), m.me());
    symlink(m.path("config.json"), m.path("link.json")).unwrap();
    let out = m
        .command_with(&m.path("link.json"), SWITCH, &["--toplevel", &two])
        .output()
        .unwrap();
    assert_eq!(said(&out).0, Some(0), "{:?}", said(&out));
    let run = m.fake("systemd-run");
    assert!(run.contains(&format!(" {me} --revert-to {one}\n")), "{run}");
    assert!(
        run.contains(&format!("ExecStopPost={me} --activation-stopped {two} ")),
        "{run}"
    );
}

#[test]
fn what_isnt_a_system_or_runs_already_changes_nothing() {
    let m = Machine::new(false);
    for path in [m.path("flake"), m.path("nowhere")] {
        let shown = path.display().to_string();
        assert_eq!(
            said(&m.switch(&["--toplevel", &shown])),
            refused(1, &format!("not a NixOS system: {shown}\n"))
        );
    }
    // Through a link, as a profile or a result would be.
    std::os::unix::fs::symlink(m.path("systems/one"), m.path("result")).unwrap();
    let out = m.switch(&["--toplevel", &m.path("result").display().to_string()]);
    assert_eq!(
        said(&out),
        ok(&format!("already running {}\n", m.sys("one")))
    );
    assert_eq!(m.fake("systemd-run"), "");
    assert!(!m.path("state").exists());
}

#[test]
fn a_system_from_an_older_nixpkgs_is_refused_unless_allowed() {
    let m = Machine::new(false);
    let old = m.system("old", Some(50));
    let refusal = format!(
        "refusing: {old} is built from an older nixpkgs than the running system\n(that would undo security fixes; pull origin/main first, or pass --allow-downgrade)\n"
    );
    assert_eq!(said(&m.switch(&["--toplevel", &old])), refused(3, &refusal));
    // One that doesn't say is from no time at all.
    let unsaid = m.system("unsaid", None);
    assert_eq!(said(&m.switch(&["--toplevel", &unsaid])).0, Some(3));
    assert_eq!(m.fake("systemd-run"), "");
    assert!(!m.path("state").exists());
    let out = m.switch(&["--allow-downgrade", "--toplevel", &old]);
    assert_eq!(said(&out).0, Some(0));
    assert_eq!(m.running(), old);
    // The same day, or a newer one, goes.
    let same = m.system("same", Some(50));
    assert_eq!(said(&m.switch(&["--toplevel", &same])).0, Some(0));
}

#[test]
fn a_switch_builds_the_flake_as_its_owner() {
    let m = Machine::new(false);
    let two = m.sys("two");
    m.set("built", &format!("{two}\n"));
    let out = m.switch(&[]);
    assert!(
        said(&out).1.starts_with(&format!("live: {two}\n")),
        "{:?}",
        said(&out)
    );
    assert_eq!(
        m.fake("nix"),
        format!(
            "build --no-link --print-out-paths {}#nixosConfigurations.box.config.system.build.toplevel\n",
            m.path("flake").display()
        )
    );
    // Its own checkout: as itself.
    assert_eq!(m.fake("runuser"), "");
    // A link to it: built where it leads.
    symlink(m.path("flake"), m.path("link")).unwrap();
    let out = m.switch(&[m.path("link").to_str().unwrap()]);
    assert_eq!(said(&out), ok(&format!("already running {two}\n")));
    assert_eq!(
        m.fake("nix"),
        m.which(&m.path("flake").display().to_string())
            .split_once(' ')
            .unwrap()
            .1
    );
    // Someone else's: as them.
    m.system("three", Some(300));
    m.set("built", &m.sys("three"));
    let owner = {
        use std::os::unix::fs::MetadataExt;
        let uid = fs::metadata("/").unwrap().uid();
        fs::read_to_string("/etc/passwd")
            .unwrap()
            .lines()
            .find(|l| l.split(':').nth(2) == Some(&uid.to_string()))
            .unwrap()
            .split(':')
            .next()
            .unwrap()
            .to_string()
    };
    let out = m.switch(&["/"]);
    assert_eq!(said(&out).0, Some(0), "{:?}", said(&out));
    let nix = m.which("/").to_string();
    assert_eq!(m.fake("runuser"), format!("-u {owner} -- {nix}"));
    // A build that fails puts nothing live.
    m.set("fail-nix", "");
    let out = m.switch(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).2.contains(" build --no-link --print-out-paths "));
    assert!(said(&out).2.ends_with(": exit status: 1\n"));
    assert_eq!(m.running(), m.sys("three"));
}

impl Machine {
    /// What the switch runs the build of the flake in `dir` with, after
    /// runuser's words.
    fn which(&self, dir: &str) -> String {
        format!(
            "{} build --no-link --print-out-paths {dir}#nixosConfigurations.box.config.system.build.toplevel\n",
            crate::machine::bin().join("nix").display()
        )
    }
}

#[test]
fn an_activation_with_errors_or_past_its_deadline_isnt_kept() {
    let m = Machine::new(false);
    let two = m.sys("two");
    m.set("exit-two", "4");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        refused(
            1,
            "activation reported errors; it will revert in 10min unless you run box-confirm\n"
        )
    );
    // Still pending: the revert takes it back on schedule.
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    // Killed at its deadline, and the revert started.
    let m = Machine::new(false);
    let one = m.sys("one");
    m.set("hang-two", "");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        refused(
            1,
            &format!(
                "not kept: activating took too long for a 10min window, so it reverts to {one}\n"
            )
        )
    );
}

fn job(id: u32, unit: &str, kind: &str, state: &str) -> String {
    format!(
        r#"[{id},"{unit}","{kind}","{state}","/org/freedesktop/systemd1/job/{id}","/unit/{unit}"]"#
    )
}

/// A sweep restarted behind a long run that was going before the
/// activation, as burrow#811's: in systemd's queue, and in what
/// switch-to-configuration lists.
fn behind_a_long_run(m: &Machine) {
    m.set(
        "jobs",
        &format!(
            "{},{}",
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting")
        ),
    );
    m.set("before-9", &job(1, "renovate.service", "start", "running"));
    m.set("lists", "renovate-sweep.service\n");
    // Nothing to wait for if it isn't let go.
    m.set("lists-ticks", "30");
}

const LET_GO: &str = "renovate-sweep.service restart waited behind renovate.service (job 1, running since before the activation): queued again, not waited for\n";

fn live(two: &str) -> String {
    format!("live: {two}\nrun box-confirm within 10min to keep it (and make it the boot default)\n")
}

/// Whether the sweep was left waiting: nothing was cancelled or queued.
fn left_waiting(m: &Machine) {
    let two = m.sys("two");
    assert_eq!(said(&m.switch(&["--toplevel", &two])), ok(&live(&two)));
    assert_eq!(m.fake("busctl"), "");
}

#[test]
fn an_activation_doesnt_wait_behind_a_run_that_was_already_going() {
    let m = Machine::new(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    behind_a_long_run(&m);
    let out = m.switch(&["--toplevel", &two]);
    // Let go, and queued again, so it still restarts after the run.
    assert_eq!(said(&out), (Some(0), live(&two), LET_GO.into()));
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nRestartUnit renovate-sweep.service replace\n"
    );
    assert_eq!(
        m.fake("journal"),
        format!("-t box-switch -p warning {LET_GO}")
    );
    // The revert's activation, too.
    let out = m.switch(&["--revert-to", &one]);
    assert_eq!(said(&out), (Some(0), String::new(), LET_GO.into()));
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nRestartUnit renovate-sweep.service replace\n"
    );
    assert_eq!(m.running(), one);
    // A job that ended before it was let go is left be.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("fail-cancel", "");
    let two = m.sys("two");
    assert_eq!(said(&m.switch(&["--toplevel", &two])), ok(&live(&two)));
    assert_eq!(m.fake("busctl"), "");
}

#[test]
fn a_job_that_has_started_or_whose_run_has_ended_since_it_was_read_is_kept() {
    // Read as waiting, started by the time it's about to be let go.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("state-9", "running");
    left_waiting(&m);
    // The run ended meanwhile.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("state-1", "done");
    left_waiting(&m);
}

#[test]
fn what_requires_a_job_or_must_follow_it_keeps_it_waited_for() {
    // A queued unit that requires the sweep would fail with its cancelling.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set(
        "RequiredBy-renovate-sweep.service",
        r#"["consumer.service"]"#,
    );
    m.set(
        "jobs",
        &format!(
            "{},{},{}",
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting"),
            job(10, "consumer.service", "start", "waiting")
        ),
    );
    left_waiting(&m);
    // Bound to it, or its requisite, the same.
    for prop in ["BoundBy", "RequisiteOf"] {
        let m = Machine::new(false);
        behind_a_long_run(&m);
        m.set(
            "jobs",
            &format!(
                "{},{},{}",
                job(1, "renovate.service", "start", "running"),
                job(9, "renovate-sweep.service", "restart", "waiting"),
                job(10, "consumer.service", "start", "waiting")
            ),
        );
        m.set(
            &format!("{prop}-renovate-sweep.service"),
            r#"["consumer.service"]"#,
        );
        left_waiting(&m);
    }
    // One with no job queued is no reason.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("RequiredBy-renovate-sweep.service", r#"["idle.service"]"#);
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
    // A service ordered after it would start before it, in the gap; a
    // target after it is let through.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("after-9", &job(10, "consumer.service", "start", "waiting"));
    left_waiting(&m);
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("after-9", &job(10, "multi-user.target", "start", "waiting"));
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
}

#[test]
fn a_target_is_not_let_go_while_it_waits_on_something_that_may_hang() {
    // multi-user.target waits on the sweep (behind the run) and on a new
    // unit that is starting: only the sweep is let go, so the switch still
    // waits for the new unit.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    // The new unit starts after the activation has begun.
    m.set(
        "jobs-later",
        &format!(
            "{},{},{},{}",
            job(1, "renovate.service", "start", "running"),
            job(9, "renovate-sweep.service", "restart", "waiting"),
            job(10, "hung.service", "start", "running"),
            job(11, "multi-user.target", "start", "waiting")
        ),
    );
    m.set("lists", "renovate-sweep.service\nmulti-user.target\n");
    m.set(
        "before-11",
        &format!(
            "{},{}",
            job(9, "renovate-sweep.service", "restart", "waiting"),
            job(10, "hung.service", "start", "running")
        ),
    );
    let two = m.sys("two");
    assert_eq!(
        said(&m.switch(&["--toplevel", &two])),
        (Some(0), live(&two), LET_GO.into())
    );
    assert_eq!(
        m.fake("busctl"),
        "cancel 9\nRestartUnit renovate-sweep.service replace\n"
    );
}

#[test]
fn a_job_that_was_running_only_after_the_activation_began_is_no_run_to_wait_behind() {
    // Job 2 waited as the activation began and runs now.
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("jobs", &job(1, "renovate.service", "start", "waiting"));
    // The look reads the queue as it begins: nothing running, nothing to see.
    m.set("lists", "app.service\n");
    m.set("before-9", &job(1, "renovate.service", "start", "running"));
    left_waiting(&m);
}

#[test]
fn a_job_let_go_that_cant_be_queued_again_fails_the_activation() {
    let m = Machine::new(false);
    behind_a_long_run(&m);
    m.set("fail-requeue", "");
    let two = m.sys("two");
    let out = m.switch(&["--toplevel", &two]);
    let (code, stdout, stderr) = said(&out);
    assert_eq!(code, Some(1));
    assert!(!stdout.contains("live:"), "{stdout}");
    assert!(
        stderr.contains("renovate-sweep.service was let go and could not be queued again: its start would be lost"),
        "{stderr}"
    );
    // Never said to be queued again.
    assert!(!stderr.contains("queued again, not waited for"), "{stderr}");
    assert!(m.fake("journal").contains("could not be queued again"));
    // The lost start stays where the next activation reads it back.
    let kept = std::fs::read_to_string(m.path("lists").join("activation-restart-list")).unwrap();
    assert!(kept.lines().any(|l| l == "renovate-sweep.service"), "{kept}");
    // The revert's activation fails the same way: it says so, and in the
    // journal with the exit the revert records.
    let out = m.switch(&["--revert-to", &m.sys("one")]);
    assert!(said(&out).2.contains("its start would be lost"));
    assert!(m.fake("journal").contains("(activation exit 127)"));
}

#[test]
fn a_revert_that_wont_arm_puts_nothing_live() {
    let m = Machine::new(false);
    m.set("fail-run-box-revert", "");
    let out = m.switch(&["--toplevel", &m.sys("two")]);
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).2.starts_with("box-switch: "));
    assert_eq!(m.fake("switch-to-configuration"), "");
    nothing_pending(&m, &m.sys("one"));
    // Nor in place of a switch that didn't go live.
    let m = Machine::new(false);
    m.set("stays-two", "");
    assert_eq!(
        m.switch(&["--toplevel", &m.sys("two")]).status.code(),
        Some(0)
    );
    let three = m.system("three", Some(300));
    m.set("fail-run-box-revert", "");
    assert_eq!(m.switch(&["--toplevel", &three]).status.code(), Some(1));
    nothing_pending(&m, &m.sys("one"));
}

/// Nothing pending, as status, abort and confirm say, and `system` runs.
fn nothing_pending(m: &Machine, system: &str) {
    assert_eq!(m.state("pending"), None);
    assert_eq!(m.state("revert-failed"), None);
    assert_eq!(m.running(), system);
    assert_eq!(
        said(&m.switch(&["--status"])),
        ok(&format!("nothing pending; running {system}\n"))
    );
    assert_eq!(said(&m.switch(&["--abort"])), ok("nothing pending\n"));
    assert_eq!(said(&m.confirm()), ok("nothing pending\n"));
}

#[test]
fn a_switch_whose_revert_wont_arm_leaves_the_one_before_it_pending_and_reverting() {
    let arm = |m: &Machine| {
        format!(
            "--unit=box-revert --on-active=10min --timer-property=AccuracySec=1s --property=Type=oneshot --property=ManagedOOMPreference=omit --setenv=PATH={} {} --revert-to {}\n",
            unit_path(),
            m.me(),
            m.sys("one")
        )
    };
    let replaced = |fails: &str| {
        let m = Machine::switched(false);
        let three = m.system("three", Some(300));
        m.set("fail-run-box-revert", fails);
        let out = m.switch(&["--toplevel", &three]);
        assert_eq!(out.status.code(), Some(1));
        (m, said(&out).2)
    };
    // Its revert armed again, with this window.
    let (m, err) = replaced("1");
    let (one, two) = (m.sys("one"), m.sys("two"));
    assert!(
        err.starts_with(&format!(
            "{two} is still pending: it reverts in 10min unless you run box-confirm\nbox-switch: "
        )),
        "{err}"
    );
    assert_eq!(m.fake("systemd-run"), arm(&m).repeat(2));
    assert_eq!(m.fake("switch-to-configuration"), "");
    assert_eq!(m.running(), two);
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(m.state("prev"), Some(format!("{one}\n")));
    assert_eq!(
        said(&m.switch(&["--status"])),
        ok(&format!(
            "pending: {two}\nreverts to: {one}\nMon 10:10 9min - - box-revert.timer box-revert.service\n"
        ))
    );
    assert_eq!(said(&m.confirm()), ok(&format!("confirmed: {two}\n")));
    assert_eq!(m.profile(), two);
    let (m, _) = replaced("1");
    assert_eq!(said(&m.switch(&["--abort"])), ok(""));
    nothing_pending(&m, &m.sys("one"));

    // When that won't arm either, it reverts now, in a unit of its own as
    // an abort does: one that outlives what started it, even when putting
    // the confirmed system back stops that.
    let now = |m: &Machine| {
        format!(
            "--unit=box-revert-now --property=ManagedOOMPreference=omit --collect --wait --pipe --quiet --setenv=PATH={} {} --revert-to {}\n",
            unit_path(),
            m.me(),
            m.sys("one")
        )
    };
    let (m, err) = replaced("");
    let (one, two) = (m.sys("one"), m.sys("two"));
    assert!(
        err.starts_with(&format!(
            "{two} is still unconfirmed, and its revert won't arm either: reverting to {one} now\nbox-switch: "
        )),
        "{err}"
    );
    assert_eq!(m.fake("systemd-run"), arm(&m).repeat(2) + &now(&m));
    assert_eq!(m.fake("switch-to-configuration"), "one test\n");
    assert_eq!(
        m.fake("journal"),
        format!("-t box-switch -p warning not confirmed, reverted to {one} (activation exit 0)\n")
    );
    nothing_pending(&m, &one);
    // One that doesn't get back says so, as a revert does.
    let m = Machine::switched(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    m.set("stays-one", "");
    m.set("fail-run-box-revert", "");
    let out = m.switch(&["--toplevel", &m.system("three", Some(300))]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(m.state("revert-failed"), Some(format!("{one}\n")));
    assert_eq!(
        m.fake("journal"),
        format!(
            "-t box-switch -p err REVERT FAILED (exit 0): still running the unconfirmed {two}\n"
        )
    );
    // And one that can't start.
    let m = Machine::switched(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    m.set("fail-run-box-revert", "");
    m.set("fail-run-box-revert-now", "");
    let out = m.switch(&["--toplevel", &m.system("three", Some(300))]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(m.fake("systemd-run"), arm(&m).repeat(2) + &now(&m));
    assert_eq!(m.fake("switch-to-configuration"), "");
    assert_eq!(m.running(), two);
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(m.state("revert-failed"), Some(format!("{one}\n")));
    assert_eq!(
        m.fake("journal"),
        format!(
            "-t box-switch -p err REVERT FAILED (it couldn't start): still running the unconfirmed {two}\n"
        )
    );
    let status = said(&m.switch(&["--status"])).1;
    assert!(
        status.starts_with(&format!("REVERT FAILED: still on the unconfirmed {two};\n")),
        "{status}"
    );

    // Its timer still waiting, when it wouldn't stop: left be.
    let m = Machine::switched(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    m.set("keeps-timer", "");
    m.set("fail-run-box-revert", "");
    let out = m.switch(&["--toplevel", &m.system("three", Some(300))]);
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).2.starts_with("box-switch: "), "{}", said(&out).2);
    assert_eq!(m.fake("systemd-run"), arm(&m));
    assert_eq!(m.fake("switch-to-configuration"), "");
    assert_eq!(m.running(), two);
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(m.state("prev"), Some(format!("{one}\n")));

    // An unconfirmed system no switch put live: left be.
    let m = Machine::new(false);
    let two = m.sys("two");
    m.runs("two");
    m.set("fail-run-box-revert", "");
    let out = m.switch(&["--toplevel", &m.system("three", Some(300))]);
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).2.starts_with("box-switch: "), "{}", said(&out).2);
    assert_eq!(m.fake("systemd-run"), arm(&m));
    assert_eq!(m.fake("switch-to-configuration"), "");
    nothing_pending(&m, &two);

    // Its revert fired as this switch stopped it, and didn't get back:
    // still said.
    let m = Machine::switched(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    m.set("activating-for", "1");
    m.set(
        "lands",
        &format!("echo {one} >{}\n", m.path("state/revert-failed").display()),
    );
    m.set("fail-run-box-revert", "");
    let out = m.switch(&["--toplevel", &m.system("three", Some(300))]);
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).2.starts_with("box-switch: "), "{}", said(&out).2);
    assert_eq!(m.fake("systemd-run"), arm(&m));
    assert_eq!(m.running(), two);
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(m.state("revert-failed"), Some(format!("{one}\n")));

    // Its revert fired and failed: still said.
    let m = Machine::switched(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    m.set("stays-one", "");
    m.set("exit-one", "1");
    assert_eq!(m.switch(&["--revert-to", &one]).status.code(), Some(1));
    m.fake("switch-to-configuration");
    m.fake("journal");
    fs::remove_file(m.path("fake/stays-one")).unwrap();
    fs::remove_file(m.path("fake/exit-one")).unwrap();
    m.set("fail-run-box-revert", "");
    let out = m.switch(&["--toplevel", &m.system("three", Some(300))]);
    assert_eq!(out.status.code(), Some(1));
    assert!(said(&out).2.starts_with("box-switch: "), "{}", said(&out).2);
    assert_eq!(m.fake("systemd-run"), arm(&m));
    assert_eq!(m.fake("switch-to-configuration"), "");
    assert_eq!(m.running(), two);
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(m.state("revert-failed"), Some(format!("{one}\n")));
    let status = said(&m.switch(&["--status"])).1;
    assert!(
        status.starts_with(&format!(
            "REVERT FAILED: still on the unconfirmed {two};\nthe last confirmed system is {one} (see journalctl -t box-switch)\npending: {two}\n"
        )),
        "{status}"
    );
}

#[test]
fn a_switch_waits_out_a_revert_already_running_and_reverts_to_the_confirmed_one() {
    let m = Machine::switched(false);
    let three = m.system("three", Some(300));
    m.set("activating-for", "1");
    let out = m.switch(&["--toplevel", &three]);
    assert_eq!(said(&out).0, Some(0));
    // Past the unconfirmed two, to the profile's one.
    assert_eq!(m.state("prev"), Some(format!("{}\n", m.sys("one"))));
    let systemctl = m.fake("systemctl");
    assert!(
        systemctl.starts_with("stop box-revert.timer\nshow -p ActiveState --value box-revert.service\nshow -p ActiveState --value box-revert.service\nreset-failed "),
        "{systemctl}"
    );
    // A revert that failed before is forgotten.
    fs::write(m.path("state/revert-failed"), m.sys("one")).unwrap();
    let four = m.system("four", Some(400));
    m.switch(&["--toplevel", &four]);
    assert_eq!(m.state("revert-failed"), None);
    // However systemd says it's still running.
    for (state, name, nixpkgs) in [("active", "five", 500), ("deactivating", "six", 600)] {
        let sys = m.system(name, Some(nixpkgs));
        m.set("activating-as", state);
        m.set("activating-for", "1");
        m.fake("systemctl");
        assert_eq!(said(&m.switch(&["--toplevel", &sys])).0, Some(0), "{state}");
        let systemctl = m.fake("systemctl");
        assert!(
            systemctl.starts_with("stop box-revert.timer\nshow -p ActiveState --value box-revert.service\nshow -p ActiveState --value box-revert.service\nreset-failed "),
            "{state}: {systemctl}"
        );
    }
}

#[test]
fn one_switch_abort_or_confirm_at_a_time() {
    let m = Machine::switched(false);
    let three = m.system("three", Some(300));
    let runs: [(&str, &[&str]); 3] = [
        (SWITCH, &["--toplevel", &three]),
        (SWITCH, &["--abort"]),
        (CONFIRM, &[]),
    ];
    for (bin, words) in runs {
        // Someone else's switch or confirm holds the state lock.
        let lock = File::create(m.path("state/lock")).unwrap();
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
        let mut run = m
            .command(bin, words)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        thread::sleep(Duration::from_millis(500));
        assert!(
            run.try_wait().unwrap().is_none(),
            "{bin} {words:?} didn't wait"
        );
        drop(lock);
        assert!(run.wait().unwrap().success(), "{bin} {words:?}");
    }
    // Each ran once it had it: three went live, the abort took it back.
    assert_eq!(m.running(), m.sys("one"));
    assert_eq!(m.state("pending"), None);
}

#[test]
fn an_empty_pending_is_nothing_pending() {
    // As a switch cut off while writing it leaves it: before the revert
    // was armed or anything went live.
    let m = Machine::new(false);
    fs::create_dir_all(m.path("state")).unwrap();
    fs::write(m.path("state/pending"), "").unwrap();
    assert_eq!(
        said(&m.switch(&["--status"])),
        ok(&format!("nothing pending; running {}\n", m.sys("one")))
    );
    assert_eq!(said(&m.switch(&["--abort"])), ok("nothing pending\n"));
    assert_eq!(said(&m.confirm()), ok("nothing pending\n"));
    assert_eq!(m.fake("systemd-run"), "");
    assert_eq!(m.fake("nix-env"), "");
}

#[test]
fn the_revert_puts_the_confirmed_system_back() {
    let m = Machine::switched(false);
    let one = m.sys("one");
    let out = m.switch(&["--revert-to", &one]);
    assert_eq!(said(&out), ok(""));
    assert_eq!(m.running(), one);
    assert_eq!(m.state("pending"), None);
    assert_eq!(m.fake("switch-to-configuration"), "one test\n");
    assert_eq!(
        m.fake("journal"),
        format!("-t box-switch -p warning not confirmed, reverted to {one} (activation exit 0)\n")
    );
    assert_eq!(
        said(&m.switch(&["--status"])),
        ok(&format!("nothing pending; running {one}\n"))
    );
    // A unit that failed to restart still counts: the system's back.
    let m = Machine::switched(false);
    m.set("exit-one", "4");
    assert_eq!(said(&m.switch(&["--revert-to", &one])).0, Some(0));
    assert!(m.fake("journal").ends_with("(activation exit 4)\n"));
    // What the journal won't take is said on stderr.
    let m = Machine::switched(false);
    m.set("fail-logger", "");
    assert_eq!(
        said(&m.switch(&["--revert-to", &one])),
        (
            Some(0),
            String::new(),
            format!("not confirmed, reverted to {one} (activation exit 0)\n")
        )
    );
}

#[test]
fn the_revert_waits_out_switch_to_configurations_lock() {
    let m = Machine::switched(false);
    let one = m.sys("one");
    // Refused as locked twice (a confirm writing the boot default, say).
    m.set("locked-one", "2");
    assert_eq!(said(&m.switch(&["--revert-to", &one])).0, Some(0));
    assert_eq!(
        m.fake("switch-to-configuration"),
        "one test\none test\none test\n"
    );
    // And held: it waits for the activation holding it.
    let m = Machine::switched(false);
    let lock = File::create(m.path("s2c.lock")).unwrap();
    if root() {
        chown(&m.path("s2c.lock"));
    }
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive).unwrap();
    let mut revert = m.command(SWITCH, &["--revert-to", &one]).spawn().unwrap();
    thread::sleep(Duration::from_millis(500));
    assert!(revert.try_wait().unwrap().is_none(), "it didn't wait");
    assert_eq!(m.running(), m.sys("two"));
    drop(lock);
    assert!(revert.wait().unwrap().success());
    assert_eq!(m.running(), one);
}

#[test]
fn a_revert_that_doesnt_get_back_says_so_and_status_too() {
    let m = Machine::switched(false);
    let (one, two) = (m.sys("one"), m.sys("two"));
    m.set("stays-one", "");
    m.set("exit-one", "1");
    let out = m.switch(&["--revert-to", &one]);
    assert_eq!(said(&out), (Some(1), String::new(), String::new()));
    assert_eq!(m.state("revert-failed"), Some(format!("{one}\n")));
    assert_eq!(m.state("pending"), Some(format!("{two}\n")));
    assert_eq!(
        m.fake("journal"),
        format!(
            "-t box-switch -p err REVERT FAILED (exit 1): still running the unconfirmed {two}\n"
        )
    );
    let out = m.switch(&["--status"]);
    assert!(
        said(&out).1.starts_with(&format!(
            "REVERT FAILED: still on the unconfirmed {two};\nthe last confirmed system is {one} (see journalctl -t box-switch)\npending: {two}\n"
        )),
        "{}",
        said(&out).1
    );
    // While the revert runs, that's what it says.
    m.set("revert-state", "activating");
    assert!(
        said(&m.switch(&["--status"]))
            .1
            .ends_with(&format!("reverting to {one} right now\n"))
    );
    // A revert that gets back later clears it.
    m.set("revert-state", "inactive");
    fs::remove_file(m.path("fake/stays-one")).unwrap();
    fs::remove_file(m.path("fake/exit-one")).unwrap();
    assert_eq!(said(&m.switch(&["--revert-to", &one])), ok(""));
    assert_eq!(m.state("revert-failed"), None);
    assert_eq!(
        said(&m.switch(&["--status"])),
        ok(&format!("nothing pending; running {one}\n"))
    );
}

#[test]
fn an_activation_killed_at_its_deadline_starts_the_revert_at_once() {
    let m = Machine::switched(false);
    let two = m.sys("two");
    // As the activation's unit runs it: with systemd's bare PATH, which on
    // NixOS has no coreutils.
    let stopped = |result: &str, system: &str| {
        let out = m
            .command(SWITCH, &["--activation-stopped", system])
            .env("SERVICE_RESULT", result)
            .env("PATH", "/var/empty")
            .output()
            .unwrap();
        assert_eq!(said(&out), ok(""));
        m.fake("systemctl")
    };
    assert_eq!(
        stopped("timeout", &two),
        "show -p SubState --value box-revert.timer\nstart --no-block box-revert.service\nstop box-revert.timer\n"
    );
    assert_eq!(
        m.fake("journal"),
        "-t box-switch -p warning activation killed: it hung past its deadline, reverting now\n"
    );
    // Stopped any other way, for a switch that isn't pending any more, or
    // once the timer's gone: left be.
    m.set("timer-state", "waiting");
    assert_eq!(stopped("success", &two), "");
    assert_eq!(stopped("timeout", &m.sys("one")), "");
    m.set("timer-state", "elapsed");
    assert_eq!(
        stopped("timeout", &two),
        "show -p SubState --value box-revert.timer\n"
    );
    assert_eq!(m.fake("journal"), "");
}

#[test]
fn an_abort_reverts_at_once() {
    let m = Machine::new(false);
    assert_eq!(said(&m.switch(&["--abort"])), ok("nothing pending\n"));
    let m = Machine::switched(false);
    let one = m.sys("one");
    let out = m.switch(&["--abort"]);
    assert_eq!(said(&out), ok(""));
    assert_eq!(
        m.fake("systemd-run"),
        format!(
            "--unit=box-revert-now --property=ManagedOOMPreference=omit --collect --wait --pipe --quiet --setenv=PATH={} {} --revert-to {one}\n",
            unit_path(),
            m.me()
        )
    );
    assert_eq!(m.running(), one);
    assert_eq!(m.state("pending"), None);
    assert!(m.fake("systemctl").starts_with("stop box-revert.timer\n"));
    // A revert that fired first lands on its own.
    let m = Machine::switched(false);
    m.set("activating-for", "1");
    m.set(
        "lands",
        &format!("rm -f {}\n", m.path("state/pending").display()),
    );
    assert_eq!(said(&m.switch(&["--abort"])), ok("already reverted\n"));
    assert_eq!(m.fake("systemd-run"), "");
    // A revert that doesn't get back exits as it did.
    let m = Machine::switched(false);
    m.set("stays-one", "");
    assert_eq!(said(&m.switch(&["--abort"])).0, Some(1));
}
