//! What the steward reads of a PR's commits: all of them, past GitHub's
//! 250, or why not.

use serde_json::{Value, json};

use super::*;
use crate::testing::{Hub, States, answer, config, look};

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FORK: &str = "ffffffffffffffffffffffffffffffffffffffff";
const DEPENDABOT: &str = "49699333+dependabot[bot]@users.noreply.github.com";
const GITHUB: &str = "noreply@github.com";

fn ask() -> Ask {
    Ask {
        head: HEAD.into(),
        base: "main".into(),
        bots: vec![DEPENDABOT.into()],
        committers: vec![DEPENDABOT.into(), GITHUB.into()],
    }
}

fn commit(sha: &str, message: &str, author: Value, committer: Value) -> Value {
    json!({"sha": sha, "commit": {"message": message, "author": {"email": author}, "committer": {"email": committer}}})
}

/// `n` commits Dependabot made, the last the head, with `message`.
fn bumps(n: usize, message: &str) -> Vec<Value> {
    (0..n)
        .map(|i| {
            let sha = if i == n - 1 {
                HEAD.into()
            } else {
                format!("{i:040x}")
            };
            let m = if i == n - 1 { message } else { "deps: bump" };
            commit(&sha, m, json!(DEPENDABOT), json!(GITHUB))
        })
        .collect()
}

fn page(path: usize) -> String {
    let first = format!("repos/sam/r/compare/main...{HEAD}?per_page=100");
    if path == 1 {
        first
    } else {
        format!("{first}&page={path}")
    }
}

/// GitHub's compare of the head with main, a hundred commits a page, of
/// `total` (all of `commits`, if None), paging through `commits` alone.
fn compare(hub: &mut Hub, commits: &[Value], total: Option<usize>) {
    let total = total.unwrap_or(commits.len());
    let pages: Vec<&[Value]> = if commits.is_empty() {
        vec![&[]]
    } else {
        commits.chunks(100).collect()
    };
    for (i, list) in pages.iter().enumerate() {
        let mut a = answer(
            200,
            json!({"total_commits": total, "merge_base_commit": {"sha": FORK}, "commits": list}),
        );
        a.next = (i + 1 < pages.len()).then(|| page(i + 2));
        hub.on("GET", &format!("{}$", page(i + 1)), a);
    }
}

fn read_as(hub: &mut Hub, ask: &Ask) -> Result<Read, String> {
    let dir = tempfile::tempdir().unwrap();
    let cfg = config(dir.path());
    look(&cfg, hub, &mut States::new("running"), 0.0, |p| {
        read(p, ask)
    })
    .1
}

fn whole(codex: bool, edited: bool) -> Result<Read, String> {
    Ok(Read::Whole { codex, edited })
}

#[test]
fn a_pr_past_250_commits_is_read_whole() {
    for (message, codex) in [
        ("deps: bump\n\nEngine: codex\n", true),
        ("deps: bump", false),
    ] {
        let mut hub = Hub::default();
        compare(&mut hub, &bumps(251, message), None);
        assert_eq!(read_as(&mut hub, &ask()), whole(codex, false), "{message}");
        assert_eq!(hub.paths(), [page(1), page(2), page(3)]);
    }
}

#[test]
fn a_list_short_of_its_count_is_said_to_be() {
    let mut hub = Hub::default();
    let mut commits = bumps(300, "deps: bump");
    commits.truncate(250);
    compare(&mut hub, &commits, Some(300));
    assert_eq!(
        read_as(&mut hub, &ask()),
        Ok(Read::Short(
            "GitHub lists 250 of its 300 commits, so who wrote the rest can't be read".into()
        ))
    );
}

#[test]
fn a_read_that_doesnt_hold_together_is_no_answer() {
    // The base moved between pages: another merge base, or another count.
    for (key, value) in [
        ("merge_base_commit", json!({"sha": "d".repeat(40)})),
        ("total_commits", json!(152)),
    ] {
        let mut hub = Hub::default();
        let commits = bumps(151, "deps: bump");
        compare(&mut hub, &commits, None);
        let mut second = answer(
            200,
            json!({"total_commits": 151, "merge_base_commit": {"sha": FORK}, "commits": &commits[100..]}),
        );
        second.body[key] = value;
        hub.on("GET", &format!("{}$", page(2)), second);
        let got = read_as(&mut hub, &ask());
        assert!(
            got.as_ref().is_err_and(|e| e.contains("base moved")),
            "{key}: {got:?}"
        );
    }
    // Another head's list, a commit with no message, one twice, a page
    // GitHub didn't give.
    let other = bumps(2, "deps: bump")
        .into_iter()
        .map(|mut c| {
            c["sha"] = json!(format!("{:040x}", 7));
            c
        })
        .take(1)
        .collect::<Vec<_>>();
    let unmessaged = vec![json!({"sha": HEAD, "commit": {}})];
    let twice = vec![bumps(1, "x")[0].clone(), bumps(1, "x")[0].clone()];
    for commits in [other, unmessaged, twice] {
        let mut hub = Hub::default();
        compare(&mut hub, &commits, None);
        assert!(read_as(&mut hub, &ask()).is_err(), "{commits:?}");
    }
    let mut hub = Hub::default();
    compare(&mut hub, &bumps(151, "deps: bump"), None);
    let mut failed = answer(0, Value::Null);
    failed.why = "gh: no connection".into();
    hub.on("GET", &format!("{}$", page(2)), failed);
    assert!(read_as(&mut hub, &ask()).is_err());
}

#[test]
fn only_the_whole_engine_trailer_marks_a_codex_job() {
    for (message, codex) in [
        ("change\n\nEngine: codex\n", true),
        ("change\n\neNgInE: codex\n", true),
        ("change\n\nOriginal-engine: codex\n", false),
        ("change\n\nNote: Engine: codex is an example\n", false),
        ("change\n\nEngine: codex, once\n", false),
        (
            "change\n\nEngine: codex\n\nThat was a quoted line.\n",
            false,
        ),
    ] {
        let mut hub = Hub::default();
        compare(&mut hub, &bumps(2, message), None);
        assert_eq!(read_as(&mut hub, &ask()), whole(codex, false), "{message}");
    }
}

#[test]
fn a_missing_address_is_no_bots() {
    let co = |line: &str| format!("deps: bump\n\n{line}\n");
    for (message, author, committer, edited) in [
        ("deps: bump".into(), json!(DEPENDABOT), json!(GITHUB), false),
        (
            co(&format!("Co-authored-by: Bot <{DEPENDABOT}>")),
            json!(DEPENDABOT),
            json!(DEPENDABOT),
            false,
        ),
        (
            co("Co-authored-by: Claude <claude@remi>"),
            json!(DEPENDABOT),
            json!(GITHUB),
            true,
        ),
        (
            co("Co-authored-by: Claude"),
            json!(DEPENDABOT),
            json!(GITHUB),
            true,
        ),
        (
            co("Co-authored-by: Claude <>"),
            json!(DEPENDABOT),
            json!(GITHUB),
            true,
        ),
        (
            "deps: bump".into(),
            json!("claude@remi"),
            json!(GITHUB),
            true,
        ),
        ("deps: bump".into(), Value::Null, json!(GITHUB), true),
        ("deps: bump".into(), json!(""), json!(GITHUB), true),
        (
            "deps: bump".into(),
            json!(DEPENDABOT),
            json!("peter@remi"),
            true,
        ),
        ("deps: bump".into(), json!(DEPENDABOT), Value::Null, true),
    ] {
        let mut commits = bumps(251, "deps: bump");
        commits[250] = commit(HEAD, &message, author.clone(), committer.clone());
        let mut hub = Hub::default();
        compare(&mut hub, &commits, None);
        // An empty address among the bots' (no Renovate address set) is
        // no one's either.
        let mut a = ask();
        a.bots.push(String::new());
        a.committers.push(String::new());
        assert_eq!(
            read_as(&mut hub, &a),
            whole(false, edited),
            "{message} {author} {committer}"
        );
    }
}

#[test]
fn an_ask_names_a_head_and_a_base() {
    assert_eq!(ask().wrong(), None);
    let mut a = ask();
    a.head = "main".into();
    assert!(a.wrong().is_some());
    let mut a = ask();
    a.base = String::new();
    assert!(a.wrong().is_some());
}
