//! The push, on real git: what went live reaches main even when main moved
//! while the run went on, with the stand-ins for runuser and nix
//! (testing.rs).

use super::*;
use crate::testing::Machine;

impl Machine {
    /// A bump on top of a clean copy of main, as a run has it.
    fn bump(&self, text: &str) {
        self.at_main();
        self.commit("flake.lock", text);
    }

    /// Before each of the clone's next `n` pushes, something lands on main
    /// (module.nix, from a clone of its own).
    fn lands_before_pushes(&self, n: u32) {
        fs::write(self.path("moves"), n.to_string()).unwrap();
        let hook = format!(
            "n=$(cat '{moves}')\n\
             [ \"$n\" -gt 0 ] || exit 0\n\
             echo $((n - 1)) >'{moves}'\n\
             export HOME='{home}' GIT_CONFIG_NOSYSTEM=1\n\
             [ -d '{by}' ] || git clone -q '{origin}' '{by}' 2>/dev/null\n\
             git -C '{by}' pull -q --ff-only origin main\n\
             echo \"landed-$(git -C '{by}' rev-parse HEAD)\" >'{by}/module.nix'\n\
             git -C '{by}' add module.nix\n\
             git -C '{by}' commit -qm landed\n\
             git -C '{by}' push -q origin HEAD:main\n",
            moves = self.path("moves").display(),
            home = self.cfg.home,
            by = self.path("lander").display(),
            origin = self.path("origin.git").display(),
        );
        fs::write(self.path("work.before-push"), hook).unwrap();
    }

    fn main_has(&self, file: &str) -> String {
        self.origin(&["show", &format!("main:{file}")])
    }

    /// The trees nix built, last first.
    fn built(&self) -> Vec<String> {
        self.kept("built").lines().rev().map(String::from).collect()
    }
}

#[test]
fn what_went_live_is_main_when_nothing_moved() {
    let m = Machine::new();
    m.bump("one-a");
    let live = m.head();
    assert_eq!(push_main(&m.cfg), Ok(()));
    assert_eq!(m.main_has("flake.lock"), "one-a");
    assert_eq!(m.main(), live);
    assert_eq!(m.recorded("deployed-main"), Some(live));
    assert!(m.built().is_empty(), "built what it had built already");
}

#[test]
fn main_moved_the_bump_goes_on_top_of_what_landed_once_that_builds() {
    // Main isn't marked as running what landed, so the next run deploys it.
    let m = Machine::new();
    m.bump("one-b");
    m.lands_before_pushes(1);
    assert_eq!(push_main(&m.cfg), Ok(()));
    assert_eq!(m.main_has("flake.lock"), "one-b");
    assert!(m.main_has("module.nix").starts_with("landed-"));
    assert_eq!(m.recorded("deployed-main"), None);
    assert_eq!(
        m.built().first(),
        Some(&m.origin(&["rev-parse", "main^{tree}"])),
        "what went on main wasn't what was built"
    );

    // Twice over: it goes again.
    m.bump("one-c");
    m.lands_before_pushes(2);
    assert_eq!(push_main(&m.cfg), Ok(()));
    assert_eq!(m.main_has("flake.lock"), "one-c");
    assert_eq!(m.recorded("deployed-main"), None);
    assert_eq!(m.built().len(), 3);
}

#[test]
fn what_doesnt_build_on_what_landed_stays_off_main() {
    let m = Machine::new();
    m.bump("one-bad");
    m.lands_before_pushes(1);
    fs::write(m.home("nix-down"), "").unwrap();
    assert_eq!(
        push_main(&m.cfg),
        Err(
            "main moved and this update doesn't build on top of it; the next run bumps it afresh"
                .into()
        )
    );
    assert_eq!(m.main_has("flake.lock"), "v1");
    assert_eq!(m.recorded("deployed-main"), None);
}

#[test]
fn main_itself_has_nothing_to_put_on_what_landed() {
    // A run with nothing to bump, deploying main itself, when main moved:
    // nothing to add, nothing built, and main still isn't what it deployed.
    let m = Machine::new();
    m.at_main();
    m.lands_before_pushes(1);
    assert_eq!(push_main(&m.cfg), Ok(()));
    assert_eq!(m.recorded("deployed-main"), None);
    assert!(m.built().is_empty(), "it built with nothing of its own");
}

#[test]
fn a_bump_that_conflicts_with_what_landed_leaves_main_and_the_clone_as_they_were() {
    let m = Machine::new();
    m.bump("one-d");
    let bump = m.head();
    m.landed("flake.lock", "one-e");
    let main = m.main();
    assert_eq!(
        push_main(&m.cfg),
        Err(
            "main moved and this update doesn't go on top of it (try 1); the next run bumps it afresh"
                .into()
        )
    );
    assert_eq!(m.main(), main);
    assert!(m.left_at(&bump), "a rebase was left half done");
    assert_eq!(m.recorded("deployed-main"), None);
}

#[test]
fn it_gives_up_once_main_moved_three_times_or_cant_be_fetched() {
    let m = Machine::new();
    m.bump("one-f");
    m.lands_before_pushes(3);
    assert_eq!(
        push_main(&m.cfg),
        Err("push to main was refused 3 times; the next run bumps it afresh".into())
    );
    assert_ne!(m.main_has("flake.lock"), "one-f");
    assert_eq!(m.recorded("deployed-main"), None);

    // GitHub gone between its push and its fetch.
    let m = Machine::new();
    m.bump("one-g");
    fs::write(
        m.path("work.before-push"),
        format!(
            "mv '{0}/origin.git' '{0}/origin.gone'\n",
            m.dir.path().display()
        ),
    )
    .unwrap();
    assert_eq!(
        push_main(&m.cfg),
        Err("push to main was refused 1 times; the next run bumps it afresh".into())
    );
    assert_eq!(m.recorded("deployed-main"), None);
}
