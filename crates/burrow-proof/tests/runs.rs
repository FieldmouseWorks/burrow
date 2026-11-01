//! `run` with real git and no fence (tests/cgroups.rs has the real one):
//! each check's command in a worktree at the head, with PROOF_* set and
//! nothing of the base; its exit the verdict, the machine's failures none;
//! only the summary in its comment; every worktree gone once it's over.

mod world;

use std::fs;

use burrow_proof::run::run;
use burrow_proof::{COMMENT, Ran, summary};
use world::{Plain, World, read};

fn ran(dir: &std::path::Path) -> Ran {
    serde_json::from_str(&read(&dir.join("ran.json"))).unwrap()
}

#[test]
fn a_pass_runs_its_command_in_a_worktree_at_the_head() {
    let w = World::new();
    let out = w.path().join("out");
    fs::create_dir(&out).unwrap();
    let o = out.display();
    let cmd = format!(
        "pwd >{o}/pwd; git rev-parse HEAD >{o}/head; env | grep -E '^PROOF_' | sort >{o}/env; \
         git show origin/main:f >{o}/main; echo SECRET-LOG-LINE"
    );
    let dir = w.made("proof-sam-r-5-1", &w.record(&[("gate", &cmd)]));
    let mut fence = Plain::default();
    run(&dir, &mut fence).unwrap();
    let ran = ran(&dir);
    assert_eq!(
        ran.verdicts["gate"],
        ["success".to_owned(), "Ran here and passed".to_owned()]
    );
    assert_eq!(ran.exits["gate"], 0);
    let tree = w.trees.join("proof-sam-r-5-1-0");
    assert_eq!(read(&out.join("pwd")).trim(), tree.to_str().unwrap());
    assert_eq!(read(&out.join("head")).trim(), w.head);
    let env = read(&out.join("env"));
    let summary_at = summary(&dir, 0);
    assert_eq!(
        env.lines().collect::<Vec<_>>(),
        [
            "PROOF_CHECK=gate".to_owned(),
            format!("PROOF_HEAD={}", w.head),
            "PROOF_NUMBER=5".to_owned(),
            "PROOF_REPO=sam/r".to_owned(),
            format!("PROOF_SUMMARY={}", summary_at.display()),
        ]
    );
    // Only the head is fetched: origin/main is as the clone had it, and the
    // base is named nowhere.
    assert_eq!(read(&out.join("main")), "main\n");
    let log = read(&dir.join("log"));
    assert!(
        log.contains("SECRET-LOG-LINE") && log.contains("autoland: gate: exit 0"),
        "{log}"
    );
    assert_eq!(fence.cleared, [0]);
    assert!(w.trees_left().is_empty(), "{:?}", w.trees_left());
    assert_eq!(w.worktrees(), [w.checkout.to_str().unwrap()]);
    // A pass with nothing written has nothing to say on the PR.
    assert!(!dir.join(COMMENT).exists());
}

#[test]
fn its_exit_is_the_verdict_and_the_machines_failures_are_none() {
    for (cmd, verdict) in [
        ("exit 1", Some(1)),
        ("exit 125", Some(125)),
        ("exit 126", None),
        ("exit 127", None),
        ("no-such-command", None),
        ("exit 200", None),
        ("kill -9 $$", None),
    ] {
        let w = World::new();
        let dir = w.made("proof-sam-r-5-1", &w.record(&[("gate", cmd)]));
        let mut fence = Plain::default();
        let got = run(&dir, &mut fence);
        match verdict {
            Some(exit) => {
                assert!(got.is_ok(), "{cmd}: {got:?}");
                let ran = ran(&dir);
                assert_eq!(
                    ran.verdicts["gate"],
                    [
                        "failure".to_owned(),
                        format!("Ran here and failed: exit {exit}")
                    ],
                    "{cmd}"
                );
            }
            None => {
                let why = got.expect_err(cmd);
                assert!(why.contains("the machine failing"), "{cmd}: {why}");
                assert!(
                    !dir.join("ran.json").exists() && !dir.join(COMMENT).exists(),
                    "{cmd}"
                );
                assert!(read(&dir.join("log")).contains(&why), "{cmd}");
            }
        }
        // However it ended, its fence was cleared and its worktree is gone.
        assert_eq!(fence.cleared, [0], "{cmd}");
        assert!(w.trees_left().is_empty(), "{cmd}: {:?}", w.trees_left());
        assert_eq!(w.worktrees().len(), 1, "{cmd}");
    }
}

#[test]
fn each_check_runs_in_turn_in_a_worktree_of_its_own() {
    let w = World::new();
    let order = w.path().join("order");
    let o = order.display();
    let dir = w.made(
        "proof-sam-r-5-1",
        &w.record(&[
            ("b", &format!("echo b >>{o}; touch made-by-b")),
            ("a", &format!("echo a >>{o}; test ! -e made-by-b")),
        ]),
    );
    run(&dir, &mut Plain::default()).unwrap();
    assert_eq!(read(&order), "a\nb\n");
    let ran = ran(&dir);
    assert_eq!(ran.verdicts.keys().collect::<Vec<_>>(), ["a", "b"]);
    assert!(ran.verdicts.values().all(|v| v[0] == "success"), "{ran:?}");
}

#[test]
fn one_the_machine_failed_ends_the_run_there() {
    let w = World::new();
    let b = w.path().join("b");
    let dir = w.made(
        "proof-sam-r-5-1",
        &w.record(&[
            ("a", "kill -9 $$"),
            ("b", &format!("touch {}", b.display())),
        ]),
    );
    assert!(run(&dir, &mut Plain::default()).is_err());
    assert!(!b.exists() && !dir.join("ran.json").exists());
}

#[test]
fn a_head_that_cant_be_fetched_runs_nothing() {
    let w = World::new();
    let ran_it = w.path().join("ran");
    let mut rec = w.record(&[("gate", &format!("touch {}", ran_it.display()))]);
    rec.head = "b".repeat(40);
    let dir = w.made("proof-sam-r-5-1", &rec);
    let why = run(&dir, &mut Plain::default()).unwrap_err();
    assert!(
        why.contains(&format!("can't fetch {}", "b".repeat(40))),
        "{why}"
    );
    assert!(!ran_it.exists() && w.trees_left().is_empty());
}

#[test]
fn it_records_its_cgroup_before_its_commands() {
    let w = World::new();
    let seen = w.path().join("seen");
    let dir = w.made(
        "proof-sam-r-5-1",
        &w.record(&[(
            "gate",
            &format!(
                "cat \"$(dirname \"$PROOF_SUMMARY\")/cgroup\" >{}",
                seen.display()
            ),
        )]),
    );
    let mut fence = Plain {
        at: Some("/sys/fs/cgroup/x.service".into()),
        ..Plain::default()
    };
    run(&dir, &mut fence).unwrap();
    assert_eq!(read(&seen), "/sys/fs/cgroup/x.service");
    assert_eq!(ran(&dir).exits["gate"], 0);
}

#[test]
fn only_the_summary_goes_on_the_pr_never_the_log() {
    for (cmd, said, failed) in [
        (
            "echo SECRET-LOG-LINE; echo '## 6 of 6 passed' >\"$PROOF_SUMMARY\"",
            Some("## 6 of 6 passed"),
            false,
        ),
        ("echo SECRET-LOG-LINE; exit 3", None, true),
        (
            "echo SECRET-LOG-LINE; echo 'clippy: exit 101' >\"$PROOF_SUMMARY\"; exit 1",
            Some("clippy: exit 101"),
            true,
        ),
    ] {
        let w = World::new();
        let dir = w.made("proof-sam-r-5-1", &w.record(&[("gate", cmd)]));
        run(&dir, &mut Plain::default()).unwrap();
        let comment = read(&dir.join(COMMENT));
        assert!(!comment.contains("SECRET-LOG-LINE"), "{comment}");
        if failed {
            let exit = ran(&dir).exits["gate"];
            assert!(
                comment.starts_with(&format!(
                    "`gate` failed on {}: it ran here and exited {exit}.",
                    w.head
                )),
                "{comment}"
            );
        } else {
            assert!(
                comment.starts_with(&format!("`gate` passed on {}.", w.head)),
                "{comment}"
            );
        }
        match said {
            Some(s) => assert!(comment.contains(s), "{comment}"),
            None => assert!(
                comment.contains("in the log of run `proof-sam-r-5-1`"),
                "{comment}"
            ),
        }
    }
}

#[test]
fn a_summary_left_from_before_isnt_this_runs() {
    let w = World::new();
    let dir = w.made("proof-sam-r-5-1", &w.record(&[("gate", "true")]));
    fs::write(summary(&dir, 0), "an old summary").unwrap();
    run(&dir, &mut Plain::default()).unwrap();
    assert!(!dir.join(COMMENT).exists());
}
