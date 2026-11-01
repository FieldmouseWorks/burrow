use std::collections::BTreeSet;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use super::{sweep, wrong};

/// What two bases' names end with (reviewers.py `snap_name`).
const MAIN: &str = "aaaaaaaa";
const OLD: &str = "bbbbbbbb";

/// A snapshot's name: the PR, twelve of its head's digits, and its base's.
fn snap(key: &str, n: u64, head: char, base: &str) -> String {
    format!("pr-{key}-{n}-{}-{base}", head.to_string().repeat(12))
}

fn session(r: &str, key: &str, n: u64, base: &str) -> String {
    format!("{r}--pr-{key}-{n}-{base}")
}

struct Rig {
    _d: tempfile::TempDir,
    state: PathBuf,
    trees: PathBuf,
}

impl Rig {
    fn new() -> Rig {
        let d = tempfile::tempdir().unwrap();
        let (state, trees) = (d.path().join("state"), d.path().join("trees"));
        for dir in [
            "review/queue",
            "review/done",
            "review/snap",
            "review/sessions",
        ] {
            fs::create_dir_all(state.join(dir)).unwrap();
        }
        fs::create_dir_all(&trees).unwrap();
        Rig {
            _d: d,
            state,
            trees,
        }
    }

    /// A head's snapshot (its meta and its checkout), a reviewer's look at
    /// it queued and another's answer.
    fn head(&self, s: &str) {
        fs::create_dir_all(self.trees.join(s)).unwrap();
        self.put(&format!("snap/{s}.json"));
        self.put(&format!("queue/rival--{s}.json"));
        self.put(&format!("done/codex--{s}.json"));
    }

    fn put(&self, name: &str) {
        fs::write(self.state.join("review").join(name), "{}").unwrap();
    }

    fn path(&self, name: &str) -> PathBuf {
        self.state.join("review").join(name)
    }

    fn sweep(&self, key: &str, live: &[&str]) -> Vec<String> {
        let live = live.iter().map(|s| s.to_string()).collect();
        sweep(&self.state, &self.trees, key, &live).unwrap()
    }

    /// What's left in the queue, the answers, the metas and the checkouts.
    fn left(&self) -> Vec<String> {
        let mut out = vec![];
        for (d, dir) in [
            ("queue", self.path("queue")),
            ("done", self.path("done")),
            ("snap", self.path("snap")),
            ("trees", self.trees.clone()),
        ] {
            for x in fs::read_dir(dir).unwrap() {
                out.push(format!("{d}/{}", x.unwrap().file_name().to_string_lossy()));
            }
        }
        out.sort();
        out
    }

    fn sessions(&self) -> Vec<String> {
        let mut out: Vec<String> = fs::read_dir(self.path("sessions"))
            .unwrap()
            .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        out.sort();
        out
    }

    /// The look queued under `name`, as the reviewers' service holds it
    /// while it's on it (burrow-review's work.rs `review`).
    fn hold(&self, name: &str) -> File {
        let f = File::open(self.path(&format!("queue/{name}"))).unwrap();
        f.lock_shared().unwrap();
        f
    }
}

fn sorted(names: &[String]) -> Vec<String> {
    let mut v = names.to_vec();
    v.sort();
    v
}

#[test]
fn the_heads_no_open_pr_has_go_and_an_earlier_heads_answers_stay() {
    let r = Rig::new();
    let (gone, live) = (snap("sam_r", 5, 'd', MAIN), snap("sam_r", 5, 'a', MAIN));
    let (other, dashed) = (snap("sam_q", 1, 'e', MAIN), snap("sam_lo-k", 3, 'f', MAIN));
    // Retargeted: the same head on another base is another snapshot.
    let retargeted = snap("sam_r", 5, 'a', OLD);
    for s in [&gone, &live, &other, &dashed, &retargeted] {
        r.head(s);
    }
    // PR 5 is still open: an earlier head's look that's under way keeps its
    // snapshot, and what came back from its earlier heads stays for its
    // next pass to post.
    let running = snap("sam_r", 5, 'c', MAIN);
    fs::create_dir_all(r.trees.join(&running)).unwrap();
    r.put(&format!("snap/{running}.json"));
    r.put(&format!("queue/codex--{running}.json"));
    // And what a take that died partway left of a live head's checkout: the
    // finished one is moved into place whole, so this is never read, and
    // goes.
    fs::create_dir_all(r.trees.join(format!("{live}.new"))).unwrap();
    let held = r.hold(&format!("codex--{running}.json"));
    assert_eq!(
        r.sweep("sam_r", &[&live]),
        [format!("codex--{running}.json")]
    );
    // A repo with a dash in its name is one too.
    assert_eq!(r.sweep("sam_lo-k", &[]), Vec::<String>::new());
    drop(held);
    assert_eq!(
        r.left(),
        sorted(&[
            format!("trees/{live}"),
            format!("snap/{live}.json"),
            format!("queue/rival--{live}.json"),
            format!("done/codex--{live}.json"),
            format!("trees/{other}"),
            format!("snap/{other}.json"),
            format!("queue/rival--{other}.json"),
            format!("done/codex--{other}.json"),
            format!("trees/{running}"),
            format!("snap/{running}.json"),
            format!("queue/codex--{running}.json"),
            format!("done/codex--{gone}.json"),
            format!("done/codex--{retargeted}.json"),
        ])
    );
    // Once it's done, the next sweep takes its snapshot.
    fs::remove_file(r.path(&format!("queue/codex--{running}.json"))).unwrap();
    assert!(r.sweep("sam_r", &[&live]).is_empty());
    assert!(!r.trees.join(&running).exists());
    assert!(!r.path(&format!("snap/{running}.json")).exists());
    assert!(r.path(&format!("done/codex--{gone}.json")).exists());
    // And a PR that's gone takes everything with it, but another repo's.
    assert!(r.sweep("sam_r", &[]).is_empty());
    assert_eq!(
        r.left(),
        sorted(&[
            format!("trees/{other}"),
            format!("snap/{other}.json"),
            format!("queue/rival--{other}.json"),
            format!("done/codex--{other}.json"),
        ])
    );
}

#[test]
fn a_look_under_way_keeps_its_entry_its_snapshot_and_its_session() {
    // Whoever's code runs it (it need only hold its entry's lock): nothing
    // it writes after lands where another PR's would, and the sweep says
    // what it left. Its PR's other sessions go. The next sweep once it's
    // done takes the rest.
    let r = Rig::new();
    let running = snap("sam_r", 5, 'c', MAIN);
    fs::create_dir_all(r.trees.join(&running)).unwrap();
    r.put(&format!("snap/{running}.json"));
    r.put(&format!("queue/codex--{running}.json"));
    let (on, off) = (
        session("codex", "sam_r", 5, MAIN),
        session("rival", "sam_r", 5, MAIN),
    );
    for x in [&on, &off] {
        fs::create_dir_all(r.path("sessions").join(x)).unwrap();
    }
    let held = r.hold(&format!("codex--{running}.json"));
    assert_eq!(r.sweep("sam_r", &[]), [format!("codex--{running}.json")]);
    assert!(r.trees.join(&running).is_dir());
    assert!(r.path(&format!("snap/{running}.json")).exists());
    assert!(r.path(&format!("queue/codex--{running}.json")).exists());
    let s = r.sessions();
    assert!(s.contains(&on) && !s.contains(&off), "{s:?}");
    drop(held);
    r.put(&format!("done/codex--{running}.json")); // what came back as it ended
    fs::remove_file(r.path(&format!("queue/codex--{running}.json"))).unwrap();
    assert!(r.sweep("sam_r", &[]).is_empty());
    assert_eq!(r.left(), Vec::<String>::new());
    let s = r.sessions();
    assert_eq!(s.len(), 2);
    assert!(s.iter().all(|x| x.starts_with(".gone-")), "{s:?}");
}

#[test]
fn the_sessions_of_closed_or_retargeted_prs_are_marked_gone() {
    let r = Rig::new();
    let open = session("codex", "sam_r", 5, MAIN);
    let retargeted = session("codex", "sam_r", 5, OLD);
    let closed = session("rival", "sam_r", 6, MAIN);
    let marked = format!(".gone-17-{}", session("codex", "sam_r", 9, MAIN));
    // Other repos': one whose name has ours and a dash, one whose has ours
    // and an underscore.
    let others = [
        session("codex", "sam_q", 1, MAIN),
        session("codex", "sam_r-5", 7, MAIN),
        session("codex", "sam_r_x", 2, MAIN),
    ];
    // A name that can't take the mark: it's too long to.
    let long = format!("codex--pr-sam_r-8-{}", "a".repeat(220));
    for x in [&open, &retargeted, &closed, &marked, &long]
        .into_iter()
        .chain(&others)
    {
        fs::create_dir_all(r.path("sessions").join(x)).unwrap();
    }
    assert_eq!(
        r.sweep("sam_r", &[&snap("sam_r", 5, 'a', MAIN)]),
        vec![long.clone()]
    );
    let s = r.sessions();
    for x in [&open, &marked, &long].into_iter().chain(&others) {
        assert!(s.contains(x), "{x} went: {s:?}");
    }
    for x in [&retargeted, &closed] {
        let gone: Vec<_> = s.iter().filter(|g| g.ends_with(&format!("-{x}"))).collect();
        assert!(!s.contains(x), "{x} stayed: {s:?}");
        let [g] = gone[..] else {
            panic!("{x}: {gone:?}")
        };
        let stamp = g
            .strip_prefix(".gone-")
            .unwrap()
            .strip_suffix(&format!("-{x}"))
            .unwrap();
        assert!(
            !stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit()),
            "{g}"
        );
    }
    assert_eq!(s.len(), 3 + others.len() + 2);
}

#[test]
fn what_it_cant_read_or_tell_it_says() {
    let r = Rig::new();
    // A queue entry it can't open to tell whether a look is on it stays,
    // and so does what it reads.
    let s = snap("sam_r", 5, 'c', MAIN);
    fs::create_dir_all(r.path(&format!("queue/codex--{s}.json"))).unwrap();
    fs::create_dir_all(r.trees.join(&s)).unwrap();
    assert_eq!(r.sweep("sam_r", &[]), [format!("codex--{s}.json")]);
    assert!(r.trees.join(&s).is_dir());
    // A dir it can't read stops it, with why.
    let gone = snap("sam_r", 6, 'd', MAIN);
    r.put(&format!("snap/{gone}.json"));
    fs::remove_dir(r.path("done")).unwrap();
    fs::write(r.path("done"), "").unwrap();
    let err = sweep(&r.state, &r.trees, "sam_r", &BTreeSet::new()).unwrap_err();
    assert!(
        err.starts_with(&r.path("done").display().to_string()),
        "{err}"
    );
    assert!(r.path(&format!("snap/{gone}.json")).exists());
    fs::remove_file(r.path("done")).unwrap();
    fs::create_dir(r.path("done")).unwrap();
    let trees = Path::new("/nonexistent/trees");
    let err = sweep(&r.state, trees, "sam_r", &BTreeSet::new()).unwrap_err();
    assert!(err.starts_with("/nonexistent/trees: "), "{err}");
    assert!(!r.path(&format!("snap/{gone}.json")).exists());
}

#[test]
fn a_list_that_isnt_the_repos_open_snapshots_is_refused() {
    let set = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>();
    assert_eq!(wrong("sam_r", &set(&[])), None);
    let ok = snap("sam_r", 5, 'a', MAIN);
    assert_eq!(wrong("sam_r", &set(&[&ok])), None);
    assert_eq!(
        wrong("sam_lo-k", &set(&[&snap("sam_lo-k", 3, 'f', MAIN)])),
        None
    );
    for bad in [
        snap("sam_q", 5, 'a', MAIN),
        snap("sam_r-5", 7, 'a', MAIN),
        snap("sam_r", 5, 'A', MAIN),
        format!("pr-sam_r-5-{}-{MAIN}", "a".repeat(11)),
        format!("pr-sam_r-5-{}-aaaaaaa", "a".repeat(12)),
        format!("pr-sam_r--{}-{MAIN}", "a".repeat(12)),
        format!("pr-sam_r-x-{}-{MAIN}", "a".repeat(12)),
        format!("pr-sam_r-5-{}", "a".repeat(12)),
        format!("{ok}/.."),
        String::new(),
    ] {
        let why = wrong("sam_r", &set(&[&ok, &bad]));
        assert_eq!(
            why,
            Some(format!("{bad:?} isn't the name of a snapshot of sam_r's")),
            "{bad}"
        );
    }
}
