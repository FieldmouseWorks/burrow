//! Whole runs, on a machine of stand-ins (testing.rs): real git, a bare repo
//! for GitHub's copy, a nix whose systems are dirs stamped with their
//! commit, and a switch that points the machine's current system at one.

use std::fs;

use burrow_github::fake::Fake;
use serde_json::{Value, json};

use super::*;
use crate::testing::{Machine, lock, sha, train};

fn machine() -> Machine {
    Machine::running_main(&[("nixpkgs", &sha('a')), ("burrow", &sha('b'))])
}

/// One run, outside a train: GitHub is never asked anything. Without the
/// lock, which another test's child can hold a moment (`once`).
fn go(m: &Machine) -> u8 {
    once(&m.cfg, &mut Fake::default(), false)
}

/// How the last run ended, and what it said.
fn said(m: &Machine) -> (String, String) {
    let s = m.status();
    (
        s["result"].as_str().unwrap().into(),
        s["message"].as_str().unwrap().into(),
    )
}

fn switched(m: &Machine) -> bool {
    m.did("calls").contains("switch --toplevel")
}

/// How many VM tests ran.
fn vm_tests(m: &Machine) -> usize {
    m.kept("nix.log").matches("#checks.").count()
}

/// The prompts the fix jobs were given, each with its key's job file.
fn jobs(m: &Machine) -> String {
    m.kept("proj.log")
}

/// The flake.lock of the system that runs.
fn running_lock(m: &Machine) -> String {
    fs::read_to_string(m.path("current-system/flake.lock")).unwrap()
}

#[test]
fn nothing_new_with_main_running_is_up_to_date() {
    let m = machine();
    let was = m.running();
    // Whatever was left in its clone goes: a run starts from main as it is.
    fs::write(m.cfg.work.join("flake.lock"), "left\n").unwrap();
    fs::write(m.cfg.work.join("junk"), "").unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(m.work(&["status", "--porcelain", "--ignored"]), "");
    assert_eq!(said(&m), ("current".into(), "up to date".into()));
    assert_eq!(m.status()["running"], json!(was));
    assert!(!switched(&m) && vm_tests(&m) == 0);
    let live: Value =
        serde_json::from_slice(&fs::read(m.cfg.state.join("live.json")).unwrap()).unwrap();
    assert_eq!(live, json!({"main": m.main(), "running": was}));
    assert_eq!(
        m.did("notices"),
        "close not-on-main: box is back on main.\n"
    );
    assert!(!m.cfg.state.join("held-since").exists());
}

#[test]
fn a_bump_goes_live_healthy_and_onto_main() {
    let m = machine();
    let was = m.running();
    let main = m.main();
    m.upstream("nixpkgs", &sha('c'));
    assert_eq!(go(&m), 0);
    let moved = "nixpkgs aaaaaaa -> ccccccc";
    assert_eq!(
        said(&m),
        ("ok".into(), format!("live and confirmed: {moved}"))
    );
    // It runs the bump, which main has now, with the operator's checkout.
    let now = m.running();
    assert_ne!(now, was);
    assert_eq!(
        running_lock(&m),
        lock(&[("nixpkgs", &sha('c')), ("burrow", &sha('b'))]) + "\n"
    );
    assert_eq!(m.origin(&["rev-parse", "main~1"]), main);
    assert_eq!(
        m.origin(&["log", "-1", "--format=%B", "main"]),
        format!(
            "deps: update {moved}\n\nPushed by box-update after the build, the VM tests and the health check passed."
        )
    );
    assert_eq!(m.git(&m.path("operator"), &["rev-parse", "HEAD"]), m.main());
    assert_eq!(m.recorded("deployed"), Some(now.clone()));
    let live: Value =
        serde_json::from_slice(&fs::read(m.cfg.state.join("live.json")).unwrap()).unwrap();
    assert_eq!(live, json!({"main": m.main(), "running": now}));
    // Health snapshotted, switched with the dead man's switch, compared,
    // confirmed; the VM test on the bump's tree, which passed.
    let health = m.cfg.state.join("health-before");
    assert_eq!(
        m.did("calls"),
        format!(
            "health snapshot {h}\nswitch --toplevel {now}\nhealth compare {h} 180\nconfirm\n",
            h = health.display()
        )
    );
    assert_eq!(
        fs::read_to_string(m.cfg.state.join("logs/switch.log")).unwrap(),
        format!("switching to {now}, to confirm within 15min\na switch's own words\nconfirmed\n")
    );
    assert_eq!(vm_tests(&m), 1);
    assert!(m.kept("nix.log").contains("#checks.x86_64-linux.vm"));
    assert_eq!(m.recorded("clean-vm").as_deref(), Some("1"));
    // Everything it had found wrong, settled.
    assert_eq!(
        m.did("notices"),
        "close not-on-main: box is back on main.
close build-riding: main with its PR on top builds.
close build-inputs: Cleared by a later run.
close build-main: main builds again.
close check-vm: The VM test vm passes again.
close switch: An update went through.
close activation: An update went through.
close health: An update went through.
close untested: An update passed every VM test again.
"
    );
    assert_eq!(jobs(&m), "");

    // The next run finds main running.
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m), ("current".into(), "up to date".into()));
    assert_eq!(m.running(), now);
}

#[test]
fn a_switch_someone_has_pending_holds_it() {
    let m = machine();
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.cfg.switch_state.join("pending"), "").unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(
        said(&m),
        (
            "held".into(),
            "a box-switch is pending; trying again next run".into()
        )
    );
    assert_eq!(m.kept("nix.log"), "", "fetched nothing");
}

#[test]
fn a_box_running_changes_that_arent_on_main_holds() {
    let m = machine();
    let main_system = m.running();
    m.upstream("nixpkgs", &sha('c'));
    let hand = m.system("hand", &format!("{}-dirty", sha('d')));
    m.runs(&hand);
    assert_eq!(go(&m), 0);
    let held = (
        "held".to_string(),
        "box runs changes that aren't on main; holding (see box-update status)".to_string(),
    );
    assert_eq!(said(&m), held);
    assert!(!switched(&m));
    let since: i64 = m.recorded("held-since").unwrap().parse().unwrap();
    assert!((epoch() - since).abs() < 60);
    assert_eq!(m.did("notices"), "");
    // Six hours on, the owner hears of it.
    fs::write(
        m.cfg.state.join("held-since"),
        format!("{}\n", since - 6 * 3600 - 1),
    )
    .unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m), held);
    assert_eq!(
        m.did("notices"),
        "open not-on-main: box runs changes that aren't on main\n"
    );
    assert!(
        m.did("notice-not-on-main")
            .contains(&format!("({hand}, revision {}-dirty)", sha('d')))
    );
    // Back on main (what it deployed), it's settled and goes on.
    m.runs(&main_system);
    assert_eq!(go(&m), 0);
    assert!(!m.cfg.state.join("held-since").exists());
    assert!(
        m.did("notices")
            .contains("close not-on-main: box is back on main.\n")
    );
    assert_eq!(said(&m).0, "ok");
}

#[test]
fn an_input_that_doesnt_build_is_held_back_and_the_rest_go() {
    let m = machine();
    let main = m.main();
    m.upstream("nixpkgs", &sha('c'));
    m.upstream("burrow", &sha('e'));
    fs::write(m.home("build-breaks"), sha('e')).unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            "live and confirmed: nixpkgs aaaaaaa -> ccccccc (held back: burrow)".into()
        )
    );
    assert_eq!(
        running_lock(&m),
        lock(&[("nixpkgs", &sha('c')), ("burrow", &sha('b'))]) + "\n"
    );
    assert_eq!(m.origin(&["rev-parse", "main~1"]), main);
    let logs = m.cfg.state.join("logs");
    assert!(logs.join("build-all.log").exists() && logs.join("build-burrow.log").exists());
    assert!(!logs.join("build-nixpkgs.log").exists());
    let job = jobs(&m);
    assert!(
        job.starts_with("run --pipeline homelab box-update: these updates don't build with box's flake: burrow. "),
        "{job}"
    );
    assert!(m.cfg.state.join("jobs/build-inputs").exists());
    assert!(!m.did("notices").contains("close build-inputs"));
}

#[test]
fn main_itself_not_building_is_mains_to_fix() {
    let m = machine();
    let was = m.running();
    m.upstream("nixpkgs", &sha('c'));
    // burrow's commit is in every tree, main's included.
    fs::write(m.home("build-breaks"), sha('b')).unwrap();
    assert_eq!(go(&m), 1);
    assert_eq!(
        said(&m),
        ("failed".into(), "main itself doesn't build".into())
    );
    assert_eq!(m.running(), was);
    assert!(jobs(&m).contains(&format!(
        "box-update: {} main ({}) doesn't build; the log is ",
        m.cfg.origin,
        m.main()
    )));
    assert!(
        m.did("notices")
            .ends_with("open build-main: main doesn't build\n")
    );
    // A day's memory: the bump's tree isn't built again, main is.
    let built = m.kept("built").lines().count();
    assert_eq!(go(&m), 1);
    let again: Vec<String> = m
        .kept("built")
        .lines()
        .skip(built)
        .map(String::from)
        .collect();
    assert_eq!(again, [m.work(&["rev-parse", "HEAD^{tree}"])]);
    // Forced, it's all tried again.
    let built = m.kept("built").lines().count();
    assert_eq!(once(&m.cfg, &mut Fake::default(), true), 1);
    assert!(m.kept("built").lines().count() - built > 1);
}

#[test]
fn a_vm_test_the_updates_break_holds_them() {
    let m = machine();
    let was = m.running();
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.home("check-breaks"), sha('c')).unwrap();
    assert_eq!(go(&m), 1);
    let failed = (
        "failed".to_string(),
        "VM test vm fails with nixpkgs aaaaaaa -> ccccccc and passes on main".to_string(),
    );
    assert_eq!(said(&m), failed);
    assert_eq!(m.running(), was);
    assert!(!switched(&m));
    // Twice on the bump, once on main.
    assert_eq!(vm_tests(&m), 3);
    let logs = m.cfg.state.join("logs");
    assert!(logs.join("check-vm.log").exists() && logs.join("check-vm-main.log").exists());
    assert!(!logs.join("check-vm.log.again").exists());
    assert!(jobs(&m).contains(
        "box-update: the VM test vm fails with these updates on top of main: nixpkgs aaaaaaa -> ccccccc. It passes on main, so the updates break it."
    ));
    assert_eq!(m.recorded("clean-vm"), None, "never passed first time");
    // Remembered for a day: not run again.
    assert_eq!(go(&m), 1);
    assert_eq!(said(&m), failed);
    assert_eq!(vm_tests(&m), 3);
    assert!(!m.did("notices").contains("open check-vm"), "not stuck yet");
    // Stuck for a day, the owner hears of it.
    fs::write(
        m.cfg.state.join("since-check-vm"),
        format!("{}\n", epoch() - 86_401),
    )
    .unwrap();
    assert_eq!(go(&m), 1);
    assert!(
        m.did("notices")
            .ends_with("open check-vm: Updates stuck: VM test vm fails\n")
    );
}

#[test]
fn a_flaky_vm_test_lets_the_update_through() {
    let m = machine();
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.home("check-flaky"), "").unwrap();
    fs::write(m.cfg.state.join("clean-vm"), "2\n").unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m).0, "ok");
    assert_eq!(vm_tests(&m), 2);
    let logs = m.cfg.state.join("logs");
    assert!(logs.join("flaky-vm.log").exists() && logs.join("check-vm.log").exists());
    assert!(!logs.join("check-vm.log.again").exists());
    assert_eq!(m.recorded("clean-vm").as_deref(), Some("0"));
    assert!(
        m.did("notices")
            .contains("open flaky-vm: VM test vm is flaky\n")
    );
    assert!(jobs(&m).contains("box-update: the VM test vm is flaky: on "));
    // Three runs in a row passing first time settle it.
    for (n, c) in [(1, 'd'), (2, 'e'), (3, 'f')] {
        assert!(!m.did("notices").contains("close flaky-vm"), "after {n}");
        m.upstream("nixpkgs", &sha(c));
        assert_eq!(go(&m), 0);
    }
    assert!(
        m.did("notices").contains(
            "close flaky-vm: The VM test vm has passed first time three runs in a row.\n"
        )
    );
}

#[test]
fn a_vm_test_main_fails_too_holds_the_updates_unless_they_fix_something_serious() {
    let m = machine();
    let was = m.running();
    m.upstream("nixpkgs", &sha('c'));
    // burrow's commit is in every tree: the test fails on main too.
    fs::write(m.home("check-breaks"), sha('b')).unwrap();
    assert_eq!(go(&m), 1);
    let holding = (
        "failed".to_string(),
        "VM test vm fails on main as well; holding nixpkgs aaaaaaa -> ccccccc until main passes"
            .to_string(),
    );
    assert_eq!(said(&m), holding);
    assert!(!switched(&m));
    assert!(jobs(&m).contains(&format!(
        "box-update: the VM test vm fails on {} main ({}) itself, not only with the updates.",
        m.cfg.origin,
        m.main()
    )));
    // vulnix not scanning is fixing nothing.
    fs::write(
        m.path("vulnix-db"),
        format!("{} CVE-1 openssl 9.8\n", sha('a')),
    )
    .unwrap();
    fs::write(m.path("vulnix-down"), "").unwrap();
    assert_eq!(go(&m), 1);
    assert_eq!(said(&m), holding);
    // What runs has a serious one the updates take away: they go live
    // without the test, saying so.
    fs::remove_file(m.path("vulnix-down")).unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m).0, "ok");
    assert_ne!(m.running(), was);
    assert_eq!(
        m.did("calls")
            .lines()
            .filter(|l| l.starts_with("vulnix"))
            .count(),
        5,
        "a scan that failed isn't followed by another"
    );
    assert!(
        m.did("notices")
            .contains("open untested: Security updates went live without a passing VM test\n")
    );
    assert!(!m.did("notices").contains("close untested"));
    assert!(
        m.did("notice-untested")
            .contains("in what box runs:\nCVE-1 openssl\nSo they")
    );
    assert_eq!(
        m.origin(&["log", "-1", "--format=%B", "main"]),
        "deps: update nixpkgs aaaaaaa -> ccccccc\n\nPushed by box-update without the VM test vm, which fails on main itself, because it fixes:\nCVE-1 openssl\nIt built and stayed healthy live."
    );
}

#[test]
fn a_switch_refused_as_a_downgrade_changes_nothing() {
    let m = machine();
    let (was, main) = (m.running(), m.main());
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.path("switch-rc"), "3").unwrap();
    assert_eq!(go(&m), 1);
    assert_eq!(
        said(&m),
        (
            "failed".into(),
            "the switch refused it as a downgrade; nothing changed".into()
        )
    );
    assert_eq!((m.running(), m.main()), (was, main));
    assert!(!m.did("calls").contains("--abort"));
    assert!(jobs(&m).contains(" as built from an older nixpkgs than the running system"));
    assert!(m.cfg.state.join("jobs/switch").exists());
}

#[test]
fn an_activation_that_fails_is_taken_back() {
    let m = machine();
    let (was, main) = (m.running(), m.main());
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.path("switch-rc"), "1").unwrap();
    assert_eq!(go(&m), 1);
    assert_eq!(
        said(&m),
        ("failed".into(), "activation failed; reverted".into())
    );
    assert_eq!((m.running(), m.main()), (was, main));
    assert!(m.did("calls").ends_with("switch --abort\n"));
    assert!(
        fs::read_to_string(m.cfg.state.join("logs/switch.log"))
            .unwrap()
            .ends_with("a switch's own words\ntaken back\n")
    );
    assert!(jobs(&m).contains("failed during activation and was reverted"));
}

#[test]
fn unhealthy_after_the_switch_goes_straight_back() {
    let m = machine();
    let (was, main) = (m.running(), m.main());
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.path("unhealthy"), "nginx: inactive\n").unwrap();
    assert_eq!(go(&m), 1);
    assert_eq!(
        said(&m),
        (
            "failed".into(),
            "unhealthy after the switch (nginx: inactive); reverted".into()
        )
    );
    assert_eq!((m.running(), m.main()), (was.clone(), main));
    assert!(m.did("calls").ends_with("switch --abort\n"));
    let new = m.recorded("previous");
    assert_eq!(new, None, "the stand-in keeps what it switched from");
    assert!(jobs(&m).contains(&format!(
        "these regressed: nginx: inactive. It reverted right away. Find out why (journalctl, the unit logs, a diff of the two systems: nix store diff-closures {was} "
    )));
    assert!(
        m.did("notices")
            .ends_with("open health: An update broke something and was rolled back\n")
    );
    // Healthy again, it goes through and settles.
    fs::remove_file(m.path("unhealthy")).unwrap();
    assert_eq!(go(&m), 0);
    assert!(
        m.did("notices")
            .contains("close health: An update went through.\n")
    );
    assert!(!m.cfg.state.join("jobs/health").exists());
    let log = fs::read_to_string(m.cfg.state.join("logs/switch.log")).unwrap();
    assert_eq!(log.matches("switching to").count(), 1, "this run's: {log}");
}

#[test]
fn a_confirm_that_fails_is_left_to_revert_by_itself() {
    let m = machine();
    let main = m.main();
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.path("confirm-fails"), "").unwrap();
    assert_eq!(go(&m), 1);
    assert_eq!(
        said(&m),
        (
            "failed".into(),
            "health was fine but box-confirm failed; it reverts by itself".into()
        )
    );
    assert_eq!(m.main(), main, "not pushed");
    assert!(!m.did("calls").contains("--abort"));
}

/// The machine running main with Claude Code pinned at `version`.
fn pinned(version: &str) -> Machine {
    let mut m = machine();
    m.cfg.manifest = Some("claude-code.json".into());
    m.commit("claude-code.json", &manifest(version).to_string());
    m.work(&["push", "-q", "origin", "HEAD:main"]);
    m.at_main();
    let system = crate::running::build(&m.cfg).unwrap();
    m.runs(&system);
    m
}

fn manifest(version: &str) -> Value {
    json!({"version": version, "platforms": {"linux-x64": {"checksum": format!("c-{version}")}}})
}

fn released(m: &Machine, version: &str) {
    let dir = m.path("releases").join(version);
    fs::create_dir_all(&dir).unwrap();
    fs::write(m.path("releases/latest"), format!("{version}\n")).unwrap();
    fs::write(dir.join("manifest.zst.json"), manifest(version).to_string()).unwrap();
}

#[test]
fn a_new_claude_code_is_asked_to_answer_once_live() {
    let m = pinned("2.1.0");
    released(&m, "2.1.0");
    m.upstream("nixpkgs", &sha('c'));
    assert_eq!(go(&m), 0);
    assert_eq!(m.kept("claude.log"), "", "the same Claude Code");

    released(&m, "2.1.5");
    assert_eq!(go(&m), 0);
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            "live and confirmed: claude-code 2.1.0 -> 2.1.5".into()
        )
    );
    assert_eq!(
        m.kept("claude.log"),
        "-p Reply with the single word OK. --max-turns 1\n"
    );
    assert_eq!(m.did("journal"), "");

    // One that doesn't answer is noted, not reverted.
    fs::write(m.home("claude-mute"), "").unwrap();
    released(&m, "2.1.6");
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m).0, "ok");
    assert_eq!(
        m.did("journal"),
        "-t box-update -p warning: warning: the new Claude Code didn't answer a one-line prompt\n"
    );
}

#[test]
fn what_it_put_live_goes_up_when_its_push_didnt() {
    let m = machine();
    let main = m.main();
    m.upstream("nixpkgs", &sha('c'));
    assert_eq!(go(&m), 0);
    let live = m.running();
    // Main as if the push had been refused.
    m.origin(&["update-ref", "refs/heads/main", &main]);
    fs::remove_file(m.path("calls")).unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            "already live and confirmed: nixpkgs aaaaaaa -> ccccccc".into()
        )
    );
    assert_eq!(m.running(), live);
    assert!(!switched(&m));
    assert_eq!(m.origin(&["rev-parse", "main~1"]), main);
}

#[test]
fn a_run_while_another_goes_does_nothing() {
    let m = machine();
    m.upstream("nixpkgs", &sha('c'));
    let held = fs::File::create(&m.cfg.lock).unwrap();
    rustix::fs::flock(&held, rustix::fs::FlockOperation::LockExclusive).unwrap();
    assert_eq!(run(&m.cfg, &mut Fake::default(), false), 0);
    assert!(!m.cfg.state.join("status.json").exists());
    assert_eq!(m.kept("nix.log"), "");
    // Without its lock, or the dirs to keep what it does (made before it
    // locks), it can't run.
    let mut m = m;
    let lock = m.cfg.lock.clone();
    m.cfg.lock = m.path("none/update.lock");
    assert_eq!(run(&m.cfg, &mut Fake::default(), false), 1);
    m.cfg.lock = lock;
    fs::write(m.path("blocked"), "").unwrap();
    m.cfg.state = m.path("blocked/state");
    assert_eq!(run(&m.cfg, &mut Fake::default(), false), 1);
    drop(held);
}

#[test]
fn a_vm_test_that_isnt_a_name_is_refused() {
    let mut m = machine();
    m.cfg.checks = vec!["vm".into(), "../x".into()];
    assert_eq!(go(&m), 2);
    assert!(!m.cfg.state.join("status.json").exists());
}

#[test]
fn a_fix_job_is_told_how_its_fix_reaches_main() {
    let mut m = machine();
    let checks = ["vm".to_string(), "boot".to_string()];
    let flake = m.cfg.flake.display().to_string();
    assert_eq!(
        rules(&m.cfg, &checks),
        format!(
            "Work in your own worktree (git -C {flake} worktree add \"$PROJ_SCRATCH/fix\" origin/main), never in {flake} itself: that checkout is the operator's. When the toplevel builds and `nix build -L .#checks.x86_64-linux.vm .#checks.x86_64-linux.boot` passes there, commit and push to main; box-update deploys main on its next run, with the dead man's switch. If the right answer is to wait for upstream, say so and change nothing. If it needs Sam, say exactly what."
        )
    );
    m.cfg.train = Some(train());
    let rules = rules(&m.cfg, &checks);
    assert!(
        rules.contains(" passes there, commit, push it to a branch of your own and open a PR against main, which the steward settles and the merge train lands (nothing else may move main); box-update deploys main"),
        "{rules}"
    );
}

#[test]
fn forced_it_builds_with_nothing_new() {
    let m = machine();
    assert_eq!(once(&m.cfg, &mut Fake::default(), true), 0);
    assert_eq!(
        said(&m),
        ("current".into(), "nothing new builds; up to date".into())
    );
    assert_eq!(m.kept("built").lines().count(), 1);
    assert!(!switched(&m));
}

#[test]
fn a_box_running_mains_system_under_another_stamp_is_on_main() {
    // Built from a commit main never had, that changes nothing a system is
    // made of, and switched to by hand.
    let m = machine();
    m.commit("README", "notes");
    let hand = crate::running::build(&m.cfg).unwrap();
    m.runs(&hand);
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m), ("current".into(), "up to date".into()));
    assert!(!m.cfg.state.join("held-since").exists());
}

#[test]
fn nothing_building_but_main_is_nothing_new() {
    let m = machine();
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.home("build-breaks"), sha('c')).unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(
        said(&m),
        (
            "current".into(),
            "nothing new builds; up to date (held back: nixpkgs)".into()
        )
    );
    assert!(!switched(&m));
}

#[test]
fn a_health_snapshot_it_cant_take_switches_nothing() {
    let m = machine();
    let was = m.running();
    m.upstream("nixpkgs", &sha('c'));
    fs::write(m.path("snapshot-fails"), "").unwrap();
    assert_eq!(go(&m), 1);
    assert!(!switched(&m));
    assert_eq!(m.running(), was);
    assert!(!m.cfg.state.join("status.json").exists());
}

/// main moved past what runs, as a merged PR moves it: nixpkgs at `c`.
fn merged(m: &Machine) -> String {
    m.landed(
        "flake.lock",
        &lock(&[("nixpkgs", &sha('c')), ("burrow", &sha('b'))]),
    );
    m.main()
}

#[test]
fn a_merged_pr_goes_live_the_same_way() {
    let m = machine();
    let main = merged(&m);
    assert_eq!(go(&m), 0);
    assert_eq!(
        said(&m),
        (
            "ok".into(),
            format!("live and confirmed: main {}", &main[..7])
        )
    );
    assert!(running_lock(&m).contains(&sha('c')));
    assert_eq!(m.main(), main, "nothing of its own to push");
    assert_eq!(vm_tests(&m), 1);
    let live: Value =
        serde_json::from_slice(&fs::read(m.cfg.state.join("live.json")).unwrap()).unwrap();
    assert_eq!(live, json!({"main": main, "running": m.running()}));
}

#[test]
fn a_vm_test_failing_on_main_itself_isnt_run_on_main_again() {
    let m = machine();
    let was = m.running();
    let main = merged(&m);
    fs::write(m.home("check-breaks"), sha('c')).unwrap();
    assert_eq!(go(&m), 1);
    assert_eq!(
        said(&m),
        (
            "failed".into(),
            format!(
                "VM test vm fails on main as well; holding main {} until main passes",
                &main[..7]
            )
        )
    );
    assert_eq!(vm_tests(&m), 2, "twice, on main");
    assert_eq!(m.running(), was);
    // Forced, with nothing remembered, the same.
    assert_eq!(once(&m.cfg, &mut Fake::default(), true), 1);
    assert_eq!(vm_tests(&m), 4);
}

#[test]
fn the_operators_checkout_mid_work_is_left_alone() {
    let m = machine();
    let op = m.path("operator");
    // A file of main's that no update changes.
    m.landed("README", "notes");
    m.git(&op, &["pull", "-q", "--ff-only"]);
    let at = m.git(&op, &["rev-parse", "HEAD"]);
    // On a branch of its own.
    m.git(&op, &["checkout", "-q", "-b", "mine"]);
    m.upstream("nixpkgs", &sha('c'));
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m).0, "ok");
    assert_eq!(m.git(&op, &["rev-parse", "HEAD"]), at);
    // On main, with changes of its own.
    m.git(&op, &["checkout", "-q", "main"]);
    fs::write(op.join("README"), "mine\n").unwrap();
    m.upstream("nixpkgs", &sha('d'));
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m).0, "ok");
    assert_eq!(m.git(&op, &["rev-parse", "HEAD"]), at);
}

#[test]
fn a_machine_without_its_clone_makes_one() {
    let m = machine();
    fs::remove_dir_all(&m.cfg.work).unwrap();
    assert_eq!(go(&m), 0);
    assert_eq!(said(&m), ("current".into(), "up to date".into()));
    assert_eq!(m.head(), m.main());
    assert_eq!(
        m.did("calls"),
        format!("install -d -o sam -g users {}\n", m.dir.path().display())
    );
}

#[path = "run_train_tests.rs"]
mod train;
