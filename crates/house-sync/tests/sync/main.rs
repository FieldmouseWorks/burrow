//! A pass against a GitHub that keeps its repos, their AGENTS.md and open
//! PRs, and a notifier that keeps what it was told: which repos a pass reads
//! and updates, and when the owner hears about it. The blocks are burrow's
//! own, house/.

mod github;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{fs, thread};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use github::{Asked, Fails, GitHub, Notices, Repo, repo};
use house_sync::blocks::House;
use house_sync::config::Config;
use house_sync::exclude::Exclude;
use house_sync::pass::{self, Ask};
use serde_json::json;

fn house() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../house")
}

fn block(name: &str) -> String {
    fs::read_to_string(house().join(format!("{name}.md"))).unwrap()
}

struct Ran {
    ok: Result<bool, String>,
    asked: Vec<Asked>,
    listed: Vec<String>,
    told: Vec<Vec<String>>,
    out: String,
}

impl Ran {
    /// The repos it opened a PR in.
    fn wrote(&self) -> Vec<String> {
        self.repos_where("POST", "/pulls")
    }

    /// The repos whose AGENTS.md it read.
    fn read(&self) -> Vec<String> {
        self.repos_where("GET", "/contents/AGENTS.md?ref=c0ffee")
    }

    fn repos_where(&self, method: &str, rest: &str) -> Vec<String> {
        let mut found: Vec<String> = self
            .asked
            .iter()
            .filter(|(m, p, _)| m == method && p.ends_with(rest))
            .map(|(_, p, _)| p.split('/').skip(1).take(2).collect::<Vec<_>>().join("/"))
            .collect();
        found.dedup();
        found
    }

    /// The AGENTS.md each PR it opened writes, by repo.
    fn written(&self) -> BTreeMap<String, String> {
        self.asked
            .iter()
            .filter(|(m, p, _)| m == "PUT" && p.ends_with("/contents/AGENTS.md"))
            .map(|(_, p, b)| {
                let b = b.as_ref().unwrap();
                assert_eq!(
                    (&b["branch"], &b["sha"]),
                    (&json!(branch()), &json!("f11e"))
                );
                let text = STANDARD.decode(b["content"].as_str().unwrap()).unwrap();
                (
                    p.split('/').skip(1).take(2).collect::<Vec<_>>().join("/"),
                    String::from_utf8(text).unwrap(),
                )
            })
            .collect()
    }

    /// The PRs it asked to close, with what it said.
    fn closed(&self) -> Vec<(u64, String)> {
        self.asked
            .iter()
            .filter(|(m, p, b)| {
                m == "PATCH" && p.contains("/pulls/") && b == &Some(json!({"state": "closed"}))
            })
            .map(|(_, p, _)| {
                let n: u64 = p.rsplit('/').next().unwrap().parse().unwrap();
                let said = self
                    .asked
                    .iter()
                    .find(|(m, q, _)| m == "POST" && q.ends_with(&format!("/issues/{n}/comments")))
                    .and_then(|(_, _, b)| b.as_ref()?["body"].as_str().map(String::from))
                    .expect("a comment before the close");
                (n, said)
            })
            .collect()
    }

    /// What the owner heard: open or close, and which notice.
    fn notices(&self) -> Vec<[&str; 2]> {
        self.told
            .iter()
            .map(|t| [t[0].as_str(), t[1].as_str()])
            .collect()
    }
}

const ALL_CLEAR: [[&str; 2]; 2] = [["close", "house-markers"], ["close", "house-sync"]];

struct Setup<'a> {
    current: PathBuf,
    exclude: &'a [&'a str],
    owners: &'a [&'a str],
    dry_run: bool,
    lock: PathBuf,
    fails: bool,
}

fn setup() -> Setup<'static> {
    Setup {
        current: house(),
        exclude: &[],
        owners: &["Sam"],
        dry_run: false,
        lock: std::env::temp_dir().join(format!("house-sync-test-{}.lock", std::process::id())),
        fails: false,
    }
}

fn config(s: &Setup) -> Config {
    Config {
        house: house(),
        current: s.current.clone(),
        lock: s.lock.clone(),
        owners: s.owners.iter().map(|o| o.to_string()).collect(),
        exclude: s
            .exclude
            .iter()
            .map(|e| Exclude::parse(e).unwrap())
            .collect(),
        notify: "/nonexistent/notify".into(),
        dry_run: s.dry_run,
    }
}

fn run_with(repos: &[Repo], ask: &Ask, s: &Setup, asked: Arc<Mutex<Vec<Asked>>>) -> Ran {
    let mut gh = GitHub {
        repos: repos.to_vec(),
        asked: asked.clone(),
        listed: vec![],
    };
    let mut notices = Notices {
        told: vec![],
        fails: s.fails,
    };
    let mut out = Vec::new();
    let ok = pass::run(&config(s), ask, &mut gh, &mut notices, &mut out);
    let asked = asked.lock().unwrap().clone();
    Ran {
        ok,
        asked,
        listed: gh.listed,
        told: notices.told,
        out: String::from_utf8(out).unwrap(),
    }
}

fn sync(repos: &[Repo], only: Option<&str>) -> Ran {
    sync_with(repos, only, &setup())
}

fn sync_with(repos: &[Repo], only: Option<&str>, s: &Setup) -> Ran {
    run_with(repos, &Ask { dry: false, only }, s, Arc::default())
}

fn rules() -> String {
    block("rules")
}

fn prs() -> String {
    block("prs")
}

fn current() -> String {
    format!(
        "# AGENTS.md\n\n{}\n## Own rules\n\n{}\nOwn PR notes.\n",
        rules(),
        prs()
    )
}

fn stale() -> String {
    current().replace("These hold in every repo.", "These held in every repo.")
}

fn stale_prs() -> String {
    current().replace("never as drafts", "as drafts")
}

fn branch() -> String {
    House::read(&house()).unwrap().branch()
}

fn everyone() -> Vec<Repo> {
    let s = stale();
    vec![
        repo("sam/a", Some(&s)),
        repo("sam/b", Some(&current())),
        Repo {
            archived: true,
            ..repo("sam/old", Some(&s))
        },
        Repo {
            fork: true,
            ..repo("sam/copy", Some(&s))
        },
        Repo {
            empty: true,
            ..repo("sam/new", None)
        },
        repo("sam/bare", None),
        repo("sam/none", Some("# AGENTS.md\n")),
        repo("eve/x", Some(&s)),
    ]
}

#[test]
fn burrows_blocks_are_whole_and_named_as_the_bash_named_them() {
    // Each file is one block, named for it, its marker lines first and
    // last, and none of its other lines looks like a marker (a pass would
    // take it for a broken block).
    let blocks = House::read(&house()).unwrap();
    let names: Vec<&str> = blocks.blocks.iter().map(|b| b.name.as_str()).collect();
    assert_eq!(names, ["prs", "rules"]);
    for b in &blocks.blocks {
        let text = String::from_utf8(b.text.clone()).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert!(b.name.bytes().all(|c| c.is_ascii_lowercase()), "{}", b.name);
        let first = lines[0]
            .strip_prefix(&format!("<!-- house:{} v", b.name))
            .unwrap_or_else(|| panic!("{}: {}", b.name, lines[0]));
        let digits = first.bytes().take_while(u8::is_ascii_digit).count();
        assert!(
            digits > 0 && first[digits..].starts_with(" begin. ") && first.ends_with(" -->"),
            "{}",
            lines[0]
        );
        assert_eq!(
            lines.last().copied(),
            Some(format!("<!-- house:{} end -->", b.name).as_str())
        );
        assert!(
            !lines[1..lines.len() - 1]
                .iter()
                .any(|l| l.starts_with("<!-- house:")),
            "{}",
            b.name
        );
    }
    // The branch a repo's open house PR is on keeps its name across the
    // port: `sha256sum -- *.md | sha256sum | cut -c1-8`, as the bash had it.
    let shell = std::process::Command::new("sh")
        .args(["-c", "sha256sum -- *.md | sha256sum | cut -c1-8"])
        .current_dir(house())
        .output()
        .unwrap();
    assert!(shell.status.success());
    assert_eq!(blocks.rev, String::from_utf8(shell.stdout).unwrap().trim());
    assert_eq!(blocks.branch(), format!("house/rules-{}", blocks.rev));
}

#[test]
fn a_full_pass_reads_every_repo_of_the_owners() {
    let ran = sync(&everyone(), None);
    assert_eq!(ran.ok, Ok(true), "{}", ran.out);
    assert_eq!(ran.wrote(), ["sam/a"]);
    assert_eq!(ran.read(), ["sam/a", "sam/b", "sam/bare", "sam/none"]);
    assert_eq!(ran.listed, ["Sam", "Sam", "Sam"]);
    assert_eq!(ran.notices(), ALL_CLEAR);
    assert!(
        ran.out
            .contains("house-sync: sam/a: opened https://github.com/sam/a/pull/9\n"),
        "{}",
        ran.out
    );
    assert!(
        ran.out.contains("house-sync: sam/none: no house block yet"),
        "{}",
        ran.out
    );
    // The PR: from a new branch at the commit it read, onto the default
    // branch, with what it says.
    let made = ran
        .asked
        .iter()
        .find(|(m, p, _)| m == "POST" && p == "repos/sam/a/git/refs")
        .unwrap();
    assert_eq!(
        made.2,
        Some(json!({"ref": format!("refs/heads/{}", branch()), "sha": "c0ffee"}))
    );
    // A new branch needs nothing more.
    assert!(
        !ran.asked
            .iter()
            .any(|(m, p, _)| m == "PATCH" && p.contains("/git/refs/"))
    );
    let pr = ran
        .asked
        .iter()
        .find(|(m, p, _)| m == "POST" && p == "repos/sam/a/pulls")
        .unwrap();
    let pr = pr.2.as_ref().unwrap();
    assert_eq!(
        (&pr["title"], &pr["head"], &pr["base"]),
        (
            &json!("house: the house blocks, as burrow has them now"),
            &json!(branch()),
            &json!("main")
        )
    );
    let rev = &branch()["house/rules-".len()..];
    assert!(
        pr["body"]
            .as_str()
            .unwrap()
            .starts_with(&format!("burrow's house/ changed (now {rev})."))
    );
    let put = ran.asked.iter().find(|(m, _, _)| m == "PUT").unwrap();
    assert_eq!(
        put.2.as_ref().unwrap()["message"],
        json!("house: the house blocks, as burrow has them now")
    );
}

#[test]
fn one_repos_pass_reads_that_repo_alone() {
    for name in ["sam/a", "SAM/A"] {
        let ran = sync(&everyone(), Some(name));
        assert_eq!(ran.ok, Ok(true), "{}", ran.out);
        assert_eq!(ran.wrote(), ["sam/a"], "{name}");
        assert_eq!(ran.read(), ["sam/a"], "{name}");
        assert!(ran.listed.is_empty());
        assert!(ran.told.is_empty());
    }
    let ran = sync(&everyone(), Some("sam/b"));
    assert_eq!(ran.ok, Ok(true));
    assert!(ran.wrote().is_empty() && ran.told.is_empty());
    assert_eq!(ran.read(), ["sam/b"]);
}

#[test]
fn each_block_is_rewritten_on_its_own() {
    // A stale Pull requests block gets a PR as stale rules do, and the PR
    // writes every block as burrow has it, everything outside the markers
    // as it was. An empty block is a block too.
    let (rules, prs) = (rules(), prs());
    let prs_lines: Vec<&str> = prs.split_inclusive('\n').collect();
    let empty_prs = current().replace(
        &prs,
        &format!("{}{}", prs_lines[0], prs_lines[prs_lines.len() - 1]),
    );
    for agents in [
        stale_prs(),
        stale(),
        stale().replace("never as drafts", "as drafts"),
        empty_prs,
    ] {
        let ran = sync(&[repo("sam/a", Some(&agents))], Some("sam/a"));
        assert_eq!(ran.wrote(), ["sam/a"]);
        assert_eq!(ran.written(), BTreeMap::from([("sam/a".into(), current())]));
    }
    // A repo without one of the blocks yet keeps the others, and gets
    // nothing added.
    let only_rules = format!("# AGENTS.md\n\n{rules}\n## Pull requests\n\nOur own.\n");
    let ran = sync(&[repo("sam/a", Some(&only_rules))], Some("sam/a"));
    assert!(ran.wrote().is_empty());
    let ran = sync(
        &[repo(
            "sam/a",
            Some(&only_rules.replace("These hold", "These held")),
        )],
        Some("sam/a"),
    );
    assert_eq!(
        ran.written(),
        BTreeMap::from([("sam/a".into(), only_rules)])
    );
    let only_prs = format!(
        "# AGENTS.md\n\n{}",
        prs.replace("never as drafts", "as drafts")
    );
    let ran = sync(&[repo("sam/a", Some(&only_prs))], Some("sam/a"));
    assert_eq!(
        ran.written(),
        BTreeMap::from([("sam/a".into(), format!("# AGENTS.md\n\n{prs}"))])
    );
}

#[test]
fn only_a_repo_a_full_pass_would_keep() {
    // Not another owner's, an archived one, a fork or an excluded one.
    for (name, exclude) in [
        ("eve/x", &[][..]),
        ("sam/old", &[]),
        ("sam/copy", &[]),
        ("sam/a", &["sam/*"]),
        ("sam/a", &["*/a"]),
    ] {
        let s = Setup { exclude, ..setup() };
        let ran = sync_with(&everyone(), Some(name), &s);
        assert_eq!(ran.ok, Ok(true), "{name}");
        assert!(
            ran.wrote().is_empty() && ran.read().is_empty() && ran.told.is_empty(),
            "{name}"
        );
    }
    assert!(
        sync(&everyone(), Some("eve/x"))
            .out
            .contains("house-sync: eve/x: not a repo it keeps\n")
    );
    // A name in any case, a star and a question mark. Each leaves the other
    // repo in.
    for (pattern, out, kept) in [
        ("Sam/B", "sam/b", "sam/a"),
        ("sam/b*", "sam/bare", "sam/a"),
        ("s?m/?", "sam/c", "sam/cc"),
    ] {
        let patterns = [pattern];
        let s = Setup {
            exclude: &patterns,
            ..setup()
        };
        let both = [repo(out, Some(&stale())), repo(kept, Some(&stale()))];
        assert_eq!(sync_with(&both, None, &s).read(), [kept], "{pattern}");
        let one = sync_with(&both, Some(out), &s);
        assert!(one.read().is_empty(), "{pattern}, one repo");
        assert_eq!(sync_with(&both, Some(kept), &s).read(), [kept]);
    }
    // A full pass leaves out the excluded too.
    let s = Setup {
        exclude: &["sam/?"],
        ..setup()
    };
    let ran = sync_with(&everyone(), None, &s);
    assert_eq!(ran.read(), ["sam/bare", "sam/none"]);
    assert_eq!(ran.notices(), ALL_CLEAR);
}

#[test]
fn a_repo_that_fails_is_tried_again_and_the_owner_hears_from_a_full_pass() {
    // One repo that fails (it can't be read, or its AGENTS.md can't) fails
    // one repo's pass, which tells the owner nothing: the next full pass
    // does.
    for fails in [Fails::All, Fails::Below] {
        let ran = sync(
            &[Repo {
                fails,
                ..repo("sam/a", Some(&stale()))
            }],
            Some("sam/a"),
        );
        assert_eq!(ran.ok, Ok(false));
        assert!(ran.wrote().is_empty() && ran.told.is_empty());
    }
    let broken = [
        Repo {
            fails: Fails::Below,
            ..repo("sam/a", Some(&stale()))
        },
        repo("sam/b", Some(&stale())),
    ];
    let ran = sync(&broken, None);
    assert_eq!(ran.ok, Ok(true));
    assert_eq!(ran.wrote(), ["sam/b"]);
    assert_eq!(ran.notices(), [["open", "house-sync"]]);
    assert!(
        ran.told[0][3].contains(": sam/a. It tries again"),
        "{:?}",
        ran.told
    );
    assert!(ran.out.contains("house-sync: sam/a: failed, trying again next pass (GET repos/sam/a/git/ref/heads/main: HTTP 500, 3 times)"), "{}", ran.out);
    // So is an owner whose repos can't be listed; the others' go on.
    let s = Setup {
        owners: &["Sam", "nobody"],
        ..setup()
    };
    let ran = sync_with(&[repo("sam/a", Some(&stale()))], None, &s);
    assert_eq!(ran.wrote(), ["sam/a"]);
    assert_eq!(ran.notices(), [["open", "house-sync"]]);
    assert!(
        ran.told[0][3].contains(": nobody (its repo list). It"),
        "{:?}",
        ran.told
    );
}

#[test]
fn broken_markers_get_no_pr_and_a_notice() {
    // A block twice, a begin with no end, or one block's markers inside
    // another's (either way round).
    let (rules, prs) = (rules(), prs());
    let rules_lines: Vec<&str> = rules.split_inclusive('\n').collect();
    let prs_first = prs.split_inclusive('\n').next().unwrap().to_string();
    for agents in [
        stale() + &rules,
        stale_prs() + &prs,
        current().replace("<!-- house:prs end -->\n", ""),
        format!(
            "# AGENTS.md\n\n{}{prs}{}",
            rules_lines[0],
            rules_lines[1..].concat()
        ),
        format!("# AGENTS.md\n\n{prs_first}{rules}<!-- house:prs end -->\n"),
    ] {
        let mangled = [repo("sam/a", Some(&agents))];
        let ran = sync(&mangled, Some("sam/a"));
        assert_eq!(ran.ok, Ok(true));
        assert!(ran.wrote().is_empty() && ran.told.is_empty());
        assert!(
            ran.out
                .contains("house-sync: sam/a: the markers of its house:"),
            "{}",
            ran.out
        );
        let ran = sync(&mangled, None);
        assert!(ran.wrote().is_empty());
        assert_eq!(
            ran.notices(),
            [["open", "house-markers"], ["close", "house-sync"]]
        );
        assert!(
            ran.told[0][3].contains("keep them current: sam/a. Put back"),
            "{:?}",
            ran.told
        );
    }
}

#[test]
fn a_notice_that_cant_be_given_fails_the_pass() {
    let s = Setup {
        fails: true,
        ..setup()
    };
    let ran = sync_with(&everyone(), None, &s);
    assert!(ran.ok.is_err());
    assert_eq!(ran.told.len(), 1);
}

#[test]
fn status_changes_nothing() {
    let dry = Ask {
        dry: true,
        only: Some("sam/a"),
    };
    let ran = run_with(&everyone(), &dry, &setup(), Arc::default());
    assert_eq!(ran.ok, Ok(true));
    assert!(ran.wrote().is_empty() && ran.told.is_empty());
    assert_eq!(ran.read(), ["sam/a"]);
    assert!(
        ran.asked.iter().all(|(m, _, _)| m == "GET"),
        "{:?}",
        ran.asked
    );
    let rev = &branch()["house/rules-".len()..];
    assert!(
        ran.out.contains(&format!(
            "house-sync: sam/a: would open a PR updating its house blocks to {rev}\n"
        )),
        "{}",
        ran.out
    );
    // The PR it would open replaces the older ones.
    let stale_older = Repo {
        prs: vec![(7, "house/rules-0ther".into(), false)],
        ..repo("sam/a", Some(&stale()))
    };
    let ran = run_with(&[stale_older], &dry, &setup(), Arc::default());
    assert!(
        ran.out.contains(&format!(
            "house-sync: sam/a: would close #7 (Replaced by the PR for {rev}.)\n"
        )),
        "{}",
        ran.out
    );
    // A full one, or a machine whose passes are all dry: no notices either.
    let older = Repo {
        prs: vec![(7, "house/rules-0ther".into(), false)],
        ..repo("sam/b", Some(&current()))
    };
    for (ask, dry_run) in [
        (
            Ask {
                dry: true,
                only: None,
            },
            false,
        ),
        (
            Ask {
                dry: false,
                only: None,
            },
            true,
        ),
    ] {
        let s = Setup { dry_run, ..setup() };
        let ran = run_with(std::slice::from_ref(&older), &ask, &s, Arc::default());
        assert_eq!(ran.ok, Ok(true));
        assert!(ran.asked.iter().all(|(m, _, _)| m == "GET") && ran.told.is_empty());
        assert!(ran.out.contains("house-sync: sam/b: would close #7 (AGENTS.md on main already has the current house blocks"), "{}", ran.out);
    }
}

#[test]
fn one_pass_at_a_time() {
    // While another holds the lock, a pass waits, full or one repo's, and
    // starts once it's free. status doesn't wait: it changes nothing.
    let lock = std::env::temp_dir().join(format!("house-sync-lock-{}", std::process::id()));
    for only in [None, Some("sam/a")] {
        let held = fs::File::create(&lock).unwrap();
        held.lock().unwrap();
        let asked: Arc<Mutex<Vec<Asked>>> = Arc::default();
        let (seen, path) = (asked.clone(), lock.clone());
        let pass = thread::spawn(move || {
            let s = Setup {
                lock: path,
                ..setup()
            };
            run_with(&everyone(), &Ask { dry: false, only }, &s, seen)
        });
        let s = Setup {
            lock: lock.clone(),
            ..setup()
        };
        let looked = run_with(&everyone(), &Ask { dry: true, only }, &s, Arc::default());
        assert_eq!(looked.ok, Ok(true));
        assert!(!looked.read().is_empty());
        thread::sleep(Duration::from_millis(1500));
        assert!(
            asked.lock().unwrap().is_empty(),
            "{only:?}: ran while another pass held the lock"
        );
        assert!(!pass.is_finished());
        drop(held);
        let ran = pass.join().unwrap();
        assert_eq!(ran.ok, Ok(true));
        assert_eq!(ran.wrote(), ["sam/a"], "{only:?}");
    }
    fs::remove_file(&lock).unwrap();
}

#[test]
fn a_pass_for_blocks_the_system_no_longer_has_does_nothing() {
    // It began, or waited for the lock, across a switch, so it mustn't close
    // the new blocks' PR. Any block counts, and so does one more or one
    // fewer.
    let other = |change: &dyn Fn(&Path)| {
        let d = tempfile::tempdir().unwrap();
        for name in ["prs", "rules"] {
            fs::write(d.path().join(format!("{name}.md")), block(name)).unwrap();
        }
        change(d.path());
        d
    };
    let dirs = [
        other(&|d| fs::write(d.join("prs.md"), block("prs") + "More.\n").unwrap()),
        other(&|d| {
            fs::write(
                d.join("new.md"),
                "<!-- house:new v1 begin. -->\n<!-- house:new end -->\n",
            )
            .unwrap()
        }),
        other(&|d| fs::remove_file(d.join("prs.md")).unwrap()),
    ];
    let mut currents: Vec<PathBuf> = dirs.iter().map(|d| d.path().to_path_buf()).collect();
    currents.push(dirs[0].path().join("missing"));
    let mut repos = everyone();
    repos.push(Repo {
        prs: vec![(7, "house/rules-0ther".into(), false)],
        ..repo("sam/n", Some(&stale()))
    });
    for current in currents {
        for only in [None, Some("sam/a")] {
            let s = Setup {
                current: current.clone(),
                ..setup()
            };
            let ran = sync_with(&repos, only, &s);
            assert_eq!(ran.ok, Ok(true));
            assert!(
                ran.asked.is_empty() && ran.listed.is_empty() && ran.told.is_empty(),
                "{current:?} {only:?}"
            );
            assert_eq!(
                ran.out,
                "house-sync: the system has other house blocks than this pass; the pass for those does the work\n"
            );
        }
    }
    // The same blocks somewhere else are the same.
    let same = other(&|_| {});
    let s = Setup {
        current: same.path().into(),
        ..setup()
    };
    assert_eq!(sync_with(&everyone(), Some("sam/a"), &s).wrote(), ["sam/a"]);
}

#[test]
fn older_prs_are_closed_once_theres_a_newer_one() {
    // Once a newer PR is open, or the default branch has the current blocks.
    // The current one, already open, stays, and so does anyone else's PR.
    let rev = &branch()["house/rules-".len()..];
    let older = (7, "house/rules-0ther".to_string(), false);
    let current_pr = (8, branch(), false);
    let theirs = (9, "house-cleaning".to_string(), false);
    let cases = [
        (
            stale_prs(),
            vec![older.clone(), theirs.clone()],
            true,
            "Replaced by https://github.com/sam/a/pull/9.".to_string(),
        ),
        (
            current(),
            vec![older.clone(), theirs.clone()],
            false,
            format!("AGENTS.md on main already has the current house blocks ({rev})."),
        ),
        (
            stale_prs(),
            vec![older.clone(), current_pr, theirs.clone()],
            false,
            "Replaced by https://github.com/sam/a/pull/8.".to_string(),
        ),
    ];
    for (agents, prs, writes, why) in cases {
        let ran = sync(
            &[Repo {
                prs,
                ..repo("sam/a", Some(&agents))
            }],
            Some("sam/a"),
        );
        assert_eq!(ran.ok, Ok(true));
        assert_eq!(ran.closed(), [(7, why.clone())]);
        assert_eq!(ran.wrote().len(), usize::from(writes), "{why}");
        let deleted: Vec<&str> = ran
            .asked
            .iter()
            .filter(|(m, _, _)| m == "DELETE")
            .map(|(_, p, _)| p.as_str())
            .collect();
        assert_eq!(deleted, ["repos/sam/a/git/refs/heads/house/rules-0ther"]);
    }
    // Past the first page of open PRs too; a fork's is closed, its branch
    // left to its owner.
    let mut prs: Vec<(u64, String, bool)> = (1..=150)
        .map(|n| (n, format!("feature-{n}"), false))
        .collect();
    prs.push((151, "house/rules-0ther".into(), true));
    let ran = sync(
        &[Repo {
            prs,
            ..repo("sam/a", Some(&current()))
        }],
        Some("sam/a"),
    );
    assert_eq!(ran.closed().iter().map(|c| c.0).collect::<Vec<_>>(), [151]);
    assert!(!ran.asked.iter().any(|(m, _, _)| m == "DELETE"));
    // A branch that's gone already is fine.
    let gone = vec![(5, "house/rules-gone".into(), false)];
    let ran = sync(
        &[Repo {
            prs: gone,
            ..repo("sam/a", Some(&current()))
        }],
        Some("sam/a"),
    );
    assert_eq!(ran.ok, Ok(true), "{}", ran.out);
    assert_eq!(ran.closed().iter().map(|c| c.0).collect::<Vec<_>>(), [5]);
    // One GitHub refuses to delete fails the repo, after the other PRs are
    // closed, and a full pass's notice says so. A fork's isn't asked about.
    let kept = vec![
        (5, "house/rules-kept".into(), false),
        (6, "house/rules-kept".into(), true),
        older.clone(),
    ];
    let kept = Repo {
        prs: kept,
        ..repo("sam/a", Some(&current()))
    };
    let ran = sync(std::slice::from_ref(&kept), Some("sam/a"));
    assert_eq!(ran.ok, Ok(false));
    assert_eq!(
        ran.closed().iter().map(|c| c.0).collect::<Vec<_>>(),
        [5, 6, 7]
    );
    let deleted = ran.asked.iter().filter(|(m, _, _)| m == "DELETE").count();
    assert_eq!(deleted, 2, "the fork's branch stays out of it");
    assert!(ran.out.contains("house-sync: sam/a: failed, trying again next pass (DELETE repos/sam/a/git/refs/heads/house/rules-kept: HTTP 422:"), "{}", ran.out);
    let ran = sync(&[kept], None);
    assert_eq!(ran.ok, Ok(true));
    assert_eq!(ran.notices(), [["open", "house-sync"]]);
    // A close that fails fails the repo, after the others are tried, and
    // the next pass tries it again.
    let prs = vec![(6, "house/rules-01d".into(), false), older.clone()];
    let stuck = Repo {
        prs,
        stuck: vec![6],
        ..repo("sam/a", Some(&current()))
    };
    let ran = sync(&[stuck], Some("sam/a"));
    assert_eq!(ran.ok, Ok(false));
    assert_eq!(ran.closed().iter().map(|c| c.0).collect::<Vec<_>>(), [6, 7]);
    assert!(ran.out.contains("house-sync: sam/a: failed, trying again next pass (PATCH repos/sam/a/pulls/6: HTTP 403: Resource not accessible by integration)"), "{}", ran.out);
}

#[test]
fn a_branch_an_earlier_run_left_is_started_over() {
    let ran = sync(
        &[Repo {
            branch_left: true,
            ..repo("sam/a", Some(&stale()))
        }],
        Some("sam/a"),
    );
    assert_eq!(ran.ok, Ok(true), "{}", ran.out);
    let moved = ran.asked.iter().find(|(m, _, _)| m == "PATCH").unwrap();
    assert_eq!(moved.1, format!("repos/sam/a/git/refs/heads/{}", branch()));
    assert_eq!(moved.2, Some(json!({"sha": "c0ffee", "force": true})));
    assert_eq!(ran.wrote(), ["sam/a"]);
}
