//! What the machine runs against what HEAD builds, on its own: real git,
//! and the stand-in for nix (testing.rs), whose system is made of flake.lock
//! and system.nix and is stamped with the commit it's built from.

use std::path::Path;

use serde_json::json;

use super::*;
use crate::testing::{Machine, sha};

/// When the test's commits are made, give or take the seconds each one
/// says: a commit made again at another time is another commit.
const AT: u64 = 1_700_000_000;

impl Machine {
    /// `file` holding `text`, committed on what's checked out, `t` seconds
    /// into the test.
    fn commit_at(&self, t: u64, file: &str, text: &str) {
        fs::write(self.cfg.work.join(file), format!("{text}\n")).unwrap();
        self.work(&["add", file]);
        self.dated(t, &["commit", "-qm", &format!("{file}: {text}")]);
    }

    fn dated(&self, t: u64, args: &[&str]) {
        let date = format!("@{} +0000", AT + t);
        let out = self
            .git_at(&self.cfg.work)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    /// main moving at `t`, as another PR's landing would.
    fn lands(&self, t: u64, file: &str, text: &str) {
        self.work(&["checkout", "-q", "main"]);
        self.commit_at(t, file, text);
    }

    /// HEAD as a run leaves it before building main itself: main,
    /// checked out.
    fn on_main(&self) -> String {
        self.work(&["checkout", "-q", "-B", "update", "main"]);
        self.head()
    }

    /// HEAD as ride leaves it at `t`: main with the PR's head (the branch
    /// pr) merged on top.
    fn ride_at(&self, t: u64) {
        self.on_main();
        self.dated(
            t,
            &["merge", "-q", "--no-edit", "-m", "Ride #7 on main", "pr"],
        );
    }

    /// What a run that put HEAD's system live leaves: it runs, stamped with
    /// HEAD.
    fn live(&self) -> Running {
        Running {
            current: build(&self.cfg).unwrap(),
            rev: Some(self.head()),
        }
    }

    fn record(&self, name: &str, text: &str) {
        fs::write(self.cfg.state.join(name), format!("{text}\n")).unwrap();
    }

    fn forget_nix(&self) {
        fs::write(self.home("nix.log"), "").unwrap();
    }
}

/// main with flake.lock, system.nix and a README, and its PR's branch (pr)
/// bumping flake.lock on top of that, which main has since moved past (its
/// README), so a ride is a merge.
fn machine() -> Machine {
    let m = Machine::new();
    m.commit_at(0, "system.nix", "v1");
    m.commit_at(0, "README", "v1");
    m.work(&["checkout", "-q", "-b", "pr"]);
    m.commit_at(10, "flake.lock", "v2");
    m.lands(20, "README", "v2");
    m
}

#[test]
fn what_runs_is_read_as_update_sh_finds_it() {
    let a = sha('a');
    for (rev, read) in [
        (String::new(), None),
        (a.clone(), Some(a.clone())),
        (format!("{a}-dirty"), Some(format!("{a}-dirty"))),
    ] {
        let run = Running::parse("/nix/store/sys", &rev).unwrap();
        assert_eq!(
            (run.current.as_str(), run.rev),
            ("/nix/store/sys", read),
            "{rev:?}"
        );
    }
    // Anything else would go into a Nix expression as it is.
    for wrong in [
        "abc".to_string(),
        "-dirty".into(),
        "A".repeat(40),
        "b".repeat(64),
        format!("{a}-clean"),
        format!("{a}\"; in builtins.abort \"x"),
    ] {
        assert!(
            Running::parse("/nix/store/sys", &wrong).is_none(),
            "{wrong:?}"
        );
    }
}

#[test]
fn a_build_is_heads_system_with_what_nix_said_in_its_log() {
    let m = machine();
    let log = m.cfg.state.join("logs/build.log");
    fs::write(&log, "a build before\n").unwrap();
    let new = build(&m.cfg).unwrap();
    assert!(new.starts_with(m.path("store").to_str().unwrap()), "{new}");
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "",
        "kept the last build's log"
    );
    assert_eq!(
        m.kept("nix.log"),
        format!(
            "build --no-link --print-out-paths {}#nixosConfigurations.box.config.system.build.toplevel\n",
            m.cfg.work.display()
        )
    );
    // A build that names no system is none.
    fs::write(m.home("nix-says-nothing"), "").unwrap();
    assert_eq!(build(&m.cfg), None);
    fs::remove_file(m.home("nix-says-nothing")).unwrap();
    // It doesn't build: nothing, and why in its log, where same_as_running
    // adds its own.
    fs::write(m.home("nix-down"), "").unwrap();
    assert_eq!(build(&m.cfg), None);
    let run = Running {
        current: new,
        rev: None,
    };
    assert!(!same_as_running(&m.cfg, &run));
    assert_eq!(
        fs::read_to_string(&log).unwrap(),
        "error: build failed\nerror: build failed\n"
    );
    // No logs dir to say it in: nothing built.
    fs::remove_file(m.home("nix-down")).unwrap();
    fs::remove_dir_all(m.cfg.state.join("logs")).unwrap();
    assert_eq!(build(&m.cfg), None);
}

#[test]
fn heads_system_is_built_again_stamped_as_what_runs() {
    let m = machine();
    let head = m.head();
    let a = sha('a');
    for (rev, stamp) in [
        (Some(a.clone()), format!("lib.mkForce \"{a}\";")),
        (
            Some(format!("{a}-dirty")),
            format!("lib.mkForce \"{a}-dirty\";"),
        ),
        (None, "lib.mkForce null;".into()),
    ] {
        m.forget_nix();
        let run = Running {
            current: "/nix/store/sys".into(),
            rev,
        };
        assert!(!same_as_running(&m.cfg, &run));
        let asked = m.kept("nix.log");
        assert!(asked.starts_with("build --impure --no-link --print-out-paths --expr"));
        assert!(
            asked.contains(&format!(
                "builtins.getFlake \"git+file://{}?rev={head}\"",
                m.cfg.work.display()
            )),
            "{asked}"
        );
        assert!(
            asked.contains("f.nixosConfigurations.box.extendModules"),
            "{asked}"
        );
        assert!(asked.contains(&stamp), "{asked}");
    }
}

#[test]
fn a_ride_made_again_runs_already_until_mains_system_moves() {
    // The bug: riding the same PR on the same main a second time makes a
    // new merge, so its build is a new store path though nothing in it
    // differs. It runs already.
    let m = machine();
    m.ride_at(30);
    let run = m.live();
    m.ride_at(40);
    assert_ne!(
        Some(m.head()),
        run.rev,
        "the second ride is the first one's commit"
    );
    let new = build(&m.cfg).unwrap();
    assert_ne!(
        new, run.current,
        "the stand-in doesn't stamp a system with its commit"
    );
    m.forget_nix();
    assert_eq!(
        runs_already(&m.cfg, &run, &new, true, true),
        Some(Already::Current)
    );
    assert!(
        m.kept("nix.log").contains("--expr"),
        "it didn't build HEAD stamped as what runs"
    );

    // Main moves under it with what doesn't go into the system (a README, a
    // CI pin): it still runs already. With what does, it doesn't: that goes
    // live.
    m.lands(50, "README", "v3");
    m.ride_at(60);
    let new = build(&m.cfg).unwrap();
    assert!(runs_already(&m.cfg, &run, &new, true, true).is_some());
    m.lands(70, "system.nix", "v2");
    m.ride_at(80);
    let new = build(&m.cfg).unwrap();
    assert_eq!(runs_already(&m.cfg, &run, &new, true, true), None);
}

#[test]
fn a_bump_made_again_runs_already_and_a_newer_one_doesnt() {
    // A bump of its own made again, the same lock (a run before put it
    // live, but its PR didn't open or its push was refused): it runs
    // already. A bump with more in it doesn't.
    let m = machine();
    m.on_main();
    m.commit_at(90, "flake.lock", "v3");
    let run = m.live();
    m.work(&["reset", "-q", "--hard", "main"]);
    m.commit_at(100, "flake.lock", "v3");
    let new = build(&m.cfg).unwrap();
    assert!(runs_already(&m.cfg, &run, &new, true, false).is_some());
    m.work(&["reset", "-q", "--hard", "main"]);
    m.commit_at(110, "flake.lock", "v4");
    let new = build(&m.cfg).unwrap();
    assert_eq!(runs_already(&m.cfg, &run, &new, true, false), None);
}

#[test]
fn main_itself_runs_already_only_as_that_very_system() {
    // HEAD as main itself: the run asked before building, so only the very
    // system counts, and nix isn't asked again.
    let m = machine();
    m.on_main();
    let run = m.live();
    m.lands(120, "README", "v4");
    m.on_main();
    let new = build(&m.cfg).unwrap();
    m.forget_nix();
    assert_eq!(runs_already(&m.cfg, &run, &new, false, false), None);
    assert_eq!(m.kept("nix.log"), "", "it asked nix about main itself");
    assert_eq!(
        runs_already(&m.cfg, &run, &run.current, false, false),
        Some(Already::Current)
    );
}

#[test]
fn a_comparison_that_cant_be_made_never_runs_already() {
    // The run goes on to put it live, as before.
    let m = machine();
    m.ride_at(30);
    let run = m.live();
    m.ride_at(40);
    let new = build(&m.cfg).unwrap();
    fs::write(m.home("nix-down"), "").unwrap();
    assert_eq!(runs_already(&m.cfg, &run, &new, true, true), None);
    // Nor a HEAD it can't read.
    fs::remove_file(m.home("nix-down")).unwrap();
    fs::remove_dir_all(m.cfg.work.join(".git")).unwrap();
    assert!(!same_as_running(&m.cfg, &run));
}

#[test]
fn what_runs_already_goes_up_only_once_it_put_it_live_itself() {
    // A bump of its own: up only once HOST-update put that very system live
    // (the switch and the health check), in a train repo or not; held when
    // anyone else did; nothing to do while riding, or with nothing new.
    let mut m = Machine::new();
    let run = Running {
        current: "/nix/store/sys-b".into(),
        rev: None,
    };
    let already =
        |m: &Machine, made, riding| runs_already(&m.cfg, &run, &run.current, made, riding);
    assert_eq!(already(&m, true, false), Some(Already::Hold));
    m.record("deployed", "/nix/store/sys-a");
    assert_eq!(already(&m, true, false), Some(Already::Hold));
    m.record("deployed", "/nix/store/sys-b");
    assert_eq!(already(&m, true, false), Some(Already::Up));
    m.cfg.train = None;
    assert_eq!(already(&m, true, false), Some(Already::Up));
    assert_eq!(already(&m, true, true), Some(Already::Current));
    assert_eq!(already(&m, false, false), Some(Already::Current));
    assert_eq!(m.kept("nix.log"), "", "it asked nix about the very system");
}

#[test]
fn main_runs_as_the_system_that_runs_says() {
    // Main runs: stamped with it, put live as it by a run whose system
    // still runs, or the same apart from its stamp. Not once the box has
    // been switched back since to an older main's system (a rollback, a
    // trial boot's fallback), whatever the record of what a run put live
    // says.
    let m = machine();
    let main = m.on_main();
    let old = m.live();
    m.forget_nix();
    assert!(main_runs(&m.cfg, &old, &main));
    assert_eq!(
        m.kept("nix.log"),
        "",
        "it asked nix about a system stamped with main"
    );

    m.lands(140, "system.nix", "v3");
    let main = m.on_main();
    let now = m.live();
    m.record("deployed-main", &main);
    m.record("deployed", &now.current);
    // A system stamped with something else, as one put up without a switch
    // is.
    let put_up = Running {
        current: now.current.clone(),
        rev: old.rev.clone(),
    };
    assert!(main_runs(&m.cfg, &put_up, &main));
    assert!(!main_runs(&m.cfg, &old, &main), "rolled back from main");

    // Main's system, stamped with an older commit of it.
    m.lands(150, "README", "v5");
    let main = m.on_main();
    assert!(main_runs(&m.cfg, &now, &main));
}

#[test]
fn main_is_live_through_head_only_once_head_is_on_main_as_it_went_live() {
    // Otherwise only up to the main the run started from: HEAD rebased
    // before its push landed, a PR in a train repo, a push refused.
    let m = machine();
    let started = m.on_main();
    m.commit_at(160, "flake.lock", "v5");
    let head = m.head();
    m.record("deployed-main", &head);
    assert_eq!(live_through(&m.cfg, &started), head);
    m.record("deployed-main", &started);
    assert_eq!(live_through(&m.cfg, &started), started);
    fs::remove_file(m.cfg.state.join("deployed-main")).unwrap();
    assert_eq!(live_through(&m.cfg, &started), started);
}

#[test]
fn what_a_run_found_live_is_written_whole() {
    // Main's commit and the system it's in, as one JSON object, with
    // nothing half written left behind.
    let mut m = Machine::new();
    let main = m.main();
    live_at(&m.cfg, &main, "/nix/store/sys-b").unwrap();
    let live: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(m.cfg.state.join("live.json")).unwrap()).unwrap();
    assert_eq!(live, json!({"main": main, "running": "/nix/store/sys-b"}));
    assert!(!m.cfg.state.join("live.json.new").exists());
    m.cfg.state = Path::new("/nonexistent/state").into();
    let e = live_at(&m.cfg, &main, "/nix/store/sys-b").unwrap_err();
    assert!(e.starts_with("/nonexistent/state/live.json: "), "{e}");
}
