use super::*;
use burrow_github::fake::{Fake, ok};

const PIN: &str = "1111111111111111111111111111111111111111";
const HEAD: &str = "2222222222222222222222222222222222222222";
const BASE: &str = "3333333333333333333333333333333333333333";
const PR_HEAD: &str = "4444444444444444444444444444444444444444";

fn manifest(rev: &str) -> String {
    format!(
        "[workspace]\n[workspace.dependencies]\nup-core = {{ git = \"https://github.com/sam/Up\", rev = \"{rev}\" }}\n\
         [workspace.metadata.follow.\"https://github.com/sam/Up\"]\ninert = [\"flake.lock\"]\n"
    )
}

fn contents(text: &str) -> Value {
    json!({ "content": STANDARD.encode(text) })
}

/// What a pass asks of the machine, answered from a script.
#[derive(Default)]
struct FakeLocal {
    counted: Vec<String>,
    asked: Vec<String>,
}

impl Local for FakeLocal {
    fn changes(&mut self, f: &Follow, head: &str) -> Result<Vec<String>, String> {
        self.asked
            .push(format!("changes {} {}..{head}", f.upstream, f.pin));
        Ok(self.counted.clone())
    }
    fn bump(
        &mut self,
        repo: &str,
        base: &str,
        path: &str,
        f: &Follow,
        head: &str,
    ) -> Result<Vec<(String, Vec<u8>)>, String> {
        self.asked
            .push(format!("bump {repo}@{base} {path} {}", f.deps.join(",")));
        Ok(vec![
            (path.into(), manifest(head).into_bytes()),
            ("crates/Cargo.lock".into(), b"lock\n".to_vec()),
        ])
    }
}

/// sam/home follows sam/Up (PIN) in crates/Cargo.toml, and sam/Up's main
/// is at HEAD. `open` is the PR from follow/sam-up, if any: its labels,
/// and the rev its manifest pins.
fn github(open: Option<(&[&str], &str)>) -> Fake {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/home/git/ref/heads/main",
        vec![ok(200, json!({"object": {"sha": BASE}}), &[])],
    )
    .on(
        "GET",
        "repos/sam/up/git/ref/heads/main",
        vec![ok(200, json!({"object": {"sha": HEAD}}), &[])],
    )
    .on(
        "GET",
        &format!("repos/sam/home/contents/crates/Cargo.toml?ref={BASE}"),
        vec![ok(200, contents(&manifest(PIN)), &[])],
    )
    .on(
        "GET",
        "repos/sam/home/pulls?",
        vec![ok(
            200,
            match open {
                Some((labels, _)) => json!([{
                    "number": 7,
                    "head": {"sha": PR_HEAD},
                    "labels": labels.iter().map(|l| json!({"name": l})).collect::<Vec<_>>(),
                }]),
                None => json!([]),
            },
            &[],
        )],
    )
    .on(
        "GET",
        "repos/sam/home/pulls/7/commits",
        vec![ok(
            200,
            json!([{"commit": {"message": "deps: follow sam/up to 1111111"}}]),
            &[],
        )],
    )
    .on(
        "GET",
        "repos/sam/home/pulls/7",
        vec![ok(200, json!({"mergeable": true}), &[])],
    )
    .on(
        "GET",
        &format!("repos/sam/home/contents/crates/Cargo.toml?ref={PR_HEAD}"),
        vec![ok(200, contents(&manifest(open.map_or(PIN, |o| o.1))), &[])],
    )
    .on(
        "GET",
        &format!("repos/sam/home/git/commits/{BASE}"),
        vec![ok(200, json!({"tree": {"sha": "basetree"}}), &[])],
    )
    .on(
        "POST",
        "repos/sam/home/git/trees",
        vec![ok(201, json!({"sha": "newtree"}), &[])],
    )
    .on(
        "POST",
        "repos/sam/home/git/commits",
        vec![ok(201, json!({"sha": "newcommit"}), &[])],
    )
    .on(
        "POST",
        "repos/sam/home/git/refs",
        vec![ok(422, json!({"message": "Reference already exists"}), &[])],
    )
    .on(
        "PATCH",
        "repos/sam/home/git/refs/heads/follow/sam-up",
        vec![ok(200, json!({}), &[])],
    )
    .on(
        "PATCH",
        "repos/sam/home/pulls/7",
        vec![ok(200, json!({}), &[])],
    )
    .on(
        "POST",
        "repos/sam/home/pulls",
        vec![ok(
            201,
            json!({"html_url": "https://github.com/sam/home/pull/8"}),
            &[],
        )],
    )
    .on(
        "POST",
        "repos/sam/home/issues/7/comments",
        vec![ok(201, json!({}), &[])],
    )
    .on(
        "DELETE",
        "repos/sam/home/git/refs/heads/follow/sam-up",
        vec![ok(204, Value::Null, &[])],
    )
    .on(
        "GET",
        "repos/sam/home",
        vec![ok(
            200,
            json!({"full_name": "sam/home", "default_branch": "main"}),
            &[],
        )],
    )
    .on(
        "GET",
        "repos/sam/Up",
        vec![ok(
            200,
            json!({"full_name": "sam/up", "default_branch": "main"}),
            &[],
        )],
    );
    gh
}

fn cfg() -> Config {
    Config {
        manifests: vec![Manifest {
            repo: "sam/home".into(),
            path: "crates/Cargo.toml".into(),
        }],
        work: "/w".into(),
        wake: "/w/wake.json".into(),
        frozen: vec!["waiting: train".into()],
    }
}

fn pass(gh: &mut Fake, local: &mut FakeLocal, only: Option<&str>) -> (bool, String) {
    let mut out = Vec::new();
    let ran = run(&cfg(), &Ask { only, dry: false }, gh, local, &mut out);
    assert_eq!(ran.held, None);
    (ran.ok, String::from_utf8(out).unwrap())
}

fn changed(gh: &Fake) -> Vec<String> {
    gh.asked
        .iter()
        .filter(|a| a.0 != "GET")
        .map(|a| format!("{} {}", a.0, a.1))
        .collect()
}

#[test]
fn a_change_that_counts_opens_one_pr_with_the_manifest_and_lock() {
    let mut gh = github(None);
    let mut local = FakeLocal {
        counted: vec!["crates/core/src/lib.rs".into()],
        ..Default::default()
    };
    let (ok, out) = pass(&mut gh, &mut local, None);
    assert!(ok, "{out}");
    assert_eq!(
        local.asked,
        vec![
            format!("changes sam/Up {PIN}..{HEAD}"),
            format!("bump sam/home@{BASE} crates/Cargo.toml up-core"),
        ]
    );
    assert_eq!(
        changed(&gh),
        vec![
            "POST repos/sam/home/git/trees",
            "POST repos/sam/home/git/commits",
            "POST repos/sam/home/git/refs",
            "PATCH repos/sam/home/git/refs/heads/follow/sam-up",
            "POST repos/sam/home/pulls",
        ]
    );
    let tree = gh.asked("POST")[0].2.clone().unwrap();
    assert_eq!(tree["base_tree"], "basetree");
    let paths: Vec<_> = tree["tree"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths, ["crates/Cargo.toml", "crates/Cargo.lock"]);
    assert_eq!(
        gh.asked("POST")[1].2.as_ref().unwrap()["parents"],
        json!([BASE])
    );
    let pr = gh.asked("POST")[3].2.clone().unwrap();
    assert_eq!(
        (&pr["title"], &pr["head"], &pr["base"]),
        (
            &json!("deps: follow sam/up to 2222222"),
            &json!("follow/sam-up"),
            &json!("main")
        )
    );
    let body = pr["body"].as_str().unwrap();
    assert!(body.contains("- `crates/core/src/lib.rs`"), "{body}");
    assert!(body.contains(&format!("compare/{PIN}...{HEAD}")), "{body}");
    assert!(
        out.contains("https://github.com/sam/home/pull/8 moves sam/up to 2222222"),
        "{out}"
    );
}

#[test]
fn a_change_that_counts_refreshes_the_open_pr_rather_than_stack_another() {
    let mut gh = github(Some((&[], PIN)));
    let mut local = FakeLocal {
        counted: vec!["src/lib.rs".into()],
        ..Default::default()
    };
    let (ok, out) = pass(&mut gh, &mut local, None);
    assert!(ok, "{out}");
    let c = changed(&gh);
    assert_eq!(c.last().unwrap(), "PATCH repos/sam/home/pulls/7");
    assert!(
        !c.contains(&"POST repos/sam/home/pulls".to_string()),
        "{c:?}"
    );
}

#[test]
fn an_open_pr_already_at_the_head_is_left_alone() {
    let mut gh = github(Some((&[], HEAD)));
    let mut local = FakeLocal {
        counted: vec!["src/lib.rs".into()],
        ..Default::default()
    };
    let (ok, out) = pass(&mut gh, &mut local, None);
    assert!(ok, "{out}");
    assert_eq!(changed(&gh), Vec::<String>::new());
    assert!(out.contains("#7 already moves sam/up to 2222222"), "{out}");
}

#[test]
fn nothing_that_counts_opens_nothing_and_closes_what_was_open() {
    // A commit on the upstream that only moves its pin on this repo.
    let mut gh = github(None);
    let mut local = FakeLocal::default();
    let (ok, out) = pass(&mut gh, &mut local, None);
    assert!(ok, "{out}");
    assert_eq!(changed(&gh), Vec::<String>::new());
    assert_eq!(local.asked.len(), 1, "{:?}", local.asked);
    assert!(
        out.contains(
            "sam/up's main at 2222222 changes nothing crates/Cargo.toml builds from it past 1111111"
        ),
        "{out}"
    );

    let mut gh = github(Some((&[], HEAD)));
    let (ok, out) = pass(&mut gh, &mut FakeLocal::default(), None);
    assert!(ok, "{out}");
    assert_eq!(
        changed(&gh),
        vec![
            "POST repos/sam/home/issues/7/comments",
            "PATCH repos/sam/home/pulls/7",
            "DELETE repos/sam/home/git/refs/heads/follow/sam-up",
        ]
    );
}

#[test]
fn a_pr_the_train_carries_is_never_pushed_to_or_closed() {
    for counted in [vec![], vec!["src/lib.rs".to_string()]] {
        let mut gh = github(Some((&["waiting: train"], PIN)));
        let (ok, out) = pass(
            &mut gh,
            &mut FakeLocal {
                counted,
                ..Default::default()
            },
            None,
        );
        assert!(ok, "{out}");
        assert_eq!(changed(&gh), Vec::<String>::new());
        assert!(out.contains("#7 has \"waiting: train\""), "{out}");
    }
}

#[test]
fn a_push_to_another_repo_moves_nothing() {
    let mut gh = github(None);
    let mut local = FakeLocal {
        counted: vec!["src/lib.rs".into()],
        ..Default::default()
    };
    let (ok, out) = pass(&mut gh, &mut local, Some("sam/elsewhere"));
    assert!(ok, "{out}");
    assert!(local.asked.is_empty());
    assert_eq!(changed(&gh), Vec::<String>::new());
    // The upstream's push and the follower's own both start it.
    for only in ["SAM/UP", "sam/home"] {
        let mut gh = github(None);
        let mut local = FakeLocal::default();
        pass(&mut gh, &mut local, Some(only));
        assert_eq!(local.asked.len(), 1, "{only}");
    }
}

#[test]
fn a_pr_with_a_commit_a_pass_didnt_make_keeps_it() {
    // The steward's fix job made the follower build against the PR's head.
    let mut gh = github(Some((&[], PIN)));
    gh.answers.insert(
        0,
        (
            "GET".into(),
            "repos/sam/home/pulls/7/commits".into(),
            vec![ok(
                200,
                json!([
                    {"commit": {"message": "deps: follow sam/up to 1111111"}},
                    {"commit": {"message": "judge: read the new snapshot\n\nbody"}},
                ]),
                &[],
            )]
            .into(),
        ),
    );
    let mut local = FakeLocal {
        counted: vec!["src/lib.rs".into()],
        ..Default::default()
    };
    let (ok, out) = pass(&mut gh, &mut local, None);
    assert!(ok, "{out}");
    assert_eq!(changed(&gh), Vec::<String>::new());
    assert!(local.asked.len() == 1, "{:?}", local.asked);
    assert!(
        out.contains("#7 has a commit a pass didn't make (\"judge: read the new snapshot\")"),
        "{out}"
    );
}

#[test]
fn a_rate_limit_is_a_wait_not_a_failure() {
    let mut gh = github(None);
    gh.answers.insert(
        0,
        (
            "GET".into(),
            "repos/sam/Up".into(),
            vec![ok(
                403,
                json!({"message": "API rate limit exceeded"}),
                &[
                    ("x-ratelimit-remaining", "0"),
                    ("x-ratelimit-reset", "2000000000"),
                ],
            )]
            .into(),
        ),
    );
    let mut out = Vec::new();
    let ran = run(
        &cfg(),
        &Ask {
            only: None,
            dry: false,
        },
        &mut gh,
        &mut FakeLocal::default(),
        &mut out,
    );
    let out = String::from_utf8(out).unwrap();
    assert_eq!(
        ran,
        Ran {
            ok: true,
            held: Some(2_000_000_000)
        },
        "{out}"
    );
    assert!(out.contains("held off until 2000000000"), "{out}");
}
