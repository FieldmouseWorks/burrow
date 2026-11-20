//! `burrow-upkeep CONFIG token`, as its callers run it: the steward's
//! common.py for JSON, and Conary (conaryd's `token_command`, `cn`'s
//! CONARY_TOKEN_COMMAND) for `--bare`, which splits the command on
//! whitespace, adds the repo last and reads one token from stdout, exit 75
//! being "ask again later" (Conary's conary-github tokens.rs).

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use gh_token::fake::{self, Fake};

const UPKEEP: &str = env!("CARGO_BIN_EXE_burrow-upkeep");

/// GitHub as the App sees it: Sam (as GitHub spells the login) on the
/// first page of installations among 99 others, eve on the second, each
/// installation's token its own. `down`: nothing answers but a 502;
/// `page2_down`: the second page doesn't.
fn github(down: bool, page2_down: bool) -> Fake {
    fake::serve(move |r| match (r.method.as_str(), r.path.as_str()) {
        _ if down => (502, vec![], r#"{"message":"Bad Gateway"}"#.into()),
        ("GET", "/app/installations?per_page=100&page=1") => {
            let mut all = vec![(11, "Sam".to_string())];
            all.extend((0..99).map(|k| (100 + k, format!("other{k}"))));
            let all: Vec<(u64, &str)> = all.iter().map(|(i, l)| (*i, l.as_str())).collect();
            (200, vec![], fake::installations(&all))
        }
        ("GET", "/app/installations?per_page=100&page=2") if page2_down => {
            (502, vec![], r#"{"message":"Bad Gateway"}"#.into())
        }
        ("GET", "/app/installations?per_page=100&page=2") => {
            (200, vec![], fake::installations(&[(12, "eve")]))
        }
        ("POST", "/app/installations/11/access_tokens") => (
            201,
            vec![],
            r#"{"token":"ghs_sam","expires_at":"2026-10-28T23:00:00Z"}"#.into(),
        ),
        ("POST", "/app/installations/12/access_tokens") => (
            201,
            vec![],
            r#"{"token":"ghs_eve","expires_at":"2026-10-28T23:00:00Z"}"#.into(),
        ),
        _ => (404, vec![], r#"{"message":"Not Found"}"#.into()),
    })
}

/// The steward's upkeep config on `github`, as autoland.nix writes it, and
/// the App's key where systemd puts its credentials (`creds`).
fn steward(dir: &Path, github: &Fake) {
    let config = serde_json::json!({
        "stateDir": dir.join("state"),
        "proj": "proj",
        "projectsDir": dir.join("projects"),
        "owner": "Sam",
        "authors": ["sam"],
        "bots": ["renovate"],
        "app": {"slug": "steward", "id": 7},
        "api": github.url,
        "webhook": {"url": "https://example.org/burrow/hook", "events": ["push"]},
        "jobTimeout": 60,
        "renovate": null,
        "dashboard": null,
        "flaky": null,
        "settle": {
            "context": "review-settled",
            "trainContext": "conary/train",
            "rulesets": true,
            "labels": {"hold": "hold", "owner": "needs-owner", "first": "first",
                       "batch": "upstream batch", "waiting": {}},
            "defaults": {"directPush": false, "ruleset": true, "train": false},
            "repos": {}
        },
        "branches": null,
        "git": "git"
    });
    fs::write(dir.join("upkeep.json"), config.to_string()).unwrap();
    fs::create_dir_all(dir.join("creds")).unwrap();
    fs::write(dir.join("creds/app-key"), fake::key(true)).unwrap();
}

/// burrow-upkeep run with `words` after its config.
fn upkeep(dir: &Path, words: &[&str]) -> Output {
    let config = dir.join("upkeep.json").display().to_string();
    let mut argv = vec![UPKEEP, config.as_str()];
    argv.extend(words);
    run(dir, &argv)
}

/// The token command as Conary runs it: split on whitespace, the repo
/// added last.
fn conary(dir: &Path, repo: &str) -> Output {
    let command = format!(
        "{UPKEEP} {} token --bare",
        dir.join("upkeep.json").display()
    );
    let mut argv: Vec<&str> = command.split_whitespace().collect();
    argv.push(repo);
    run(dir, &argv)
}

/// `argv` run as a pass's unit runs it: its credentials given, and no
/// proxy.
fn run(dir: &Path, argv: &[&str]) -> Output {
    let mut c = Command::new(argv[0]);
    c.args(&argv[1..])
        .env("CREDENTIALS_DIRECTORY", dir.join("creds"));
    for v in [
        "HTTPS_PROXY",
        "https_proxy",
        "HTTP_PROXY",
        "http_proxy",
        "ALL_PROXY",
        "all_proxy",
    ] {
        c.env_remove(v);
    }
    c.output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn the_bare_token_is_the_one_token_mints_for_the_repos_owner() {
    let github = github(false, false);
    let dir = tempfile::tempdir().unwrap();
    steward(dir.path(), &github);
    for owner in ["sam", "eve"] {
        let json = upkeep(dir.path(), &["token", owner]);
        assert!(json.status.success(), "{}", text(&json.stderr));
        let json: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        let minted = json["token"].as_str().unwrap();
        assert_eq!(minted, format!("ghs_{owner}"));
        // As Conary runs it, and as the issue has it: the token alone,
        // the repo's owner's, in any case GitHub's names come in.
        for bare in [
            conary(dir.path(), &format!("{owner}/r")),
            upkeep(
                dir.path(),
                &["token", &format!("{}/r", owner.to_uppercase()), "--bare"],
            ),
        ] {
            assert_eq!(bare.status.code(), Some(0), "{}", text(&bare.stderr));
            assert_eq!(text(&bare.stdout), format!("{minted}\n"));
        }
    }
    // Each one minted on its installation, as the App: eve's on the second
    // page of them, read up to it.
    let asked = github.asked();
    assert_eq!(
        asked
            .iter()
            .filter(|a| *a == "POST /app/installations/11/access_tokens")
            .count(),
        3,
        "{asked:?}"
    );
    assert_eq!(
        asked
            .iter()
            .filter(|a| *a == "POST /app/installations/12/access_tokens")
            .count(),
        3,
        "{asked:?}"
    );
    // Kept nowhere: the state dir and the rest are the machine user's.
    for e in walk(dir.path()) {
        assert!(
            !text(&fs::read(&e).unwrap()).contains("ghs_"),
            "{} keeps a token",
            e.display()
        );
    }
}

#[test]
fn github_not_answering_is_exit_75_and_no_token() {
    for (down, page2_down, owner) in [(true, false, "sam"), (false, true, "eve")] {
        let github = github(down, page2_down);
        let dir = tempfile::tempdir().unwrap();
        steward(dir.path(), &github);
        for out in [
            conary(dir.path(), &format!("{owner}/r")),
            upkeep(dir.path(), &["token", owner]),
        ] {
            assert_eq!(out.status.code(), Some(75), "{}", text(&out.stderr));
            assert_eq!(text(&out.stdout), "");
            assert!(text(&out.stderr).contains("502"), "{}", text(&out.stderr));
        }
    }
}

#[test]
fn none_to_mint_is_a_failure_that_names_the_repo_and_prints_nothing() {
    let github = github(false, false);
    let dir = tempfile::tempdir().unwrap();
    steward(dir.path(), &github);
    // The App isn't on mallory: GitHub's answer, not asked again later.
    let out = conary(dir.path(), "mallory/r");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    assert!(
        text(&out.stderr).contains("isn't installed on mallory"),
        "{}",
        text(&out.stderr)
    );
    // Without the App's key (a unit not given the credential), nothing is
    // asked of GitHub.
    let asked = github.asked().len();
    fs::remove_file(dir.path().join("creds/app-key")).unwrap();
    let out = conary(dir.path(), "sam/r");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(text(&out.stdout), "");
    let err = text(&out.stderr);
    assert!(
        err.contains("no token for sam/r: ") && err.contains("the App's key"),
        "{err}"
    );
    assert_eq!(github.asked().len(), asked);
}

#[test]
fn bare_takes_one_repo_and_json_one_owner() {
    let github = github(false, false);
    let dir = tempfile::tempdir().unwrap();
    steward(dir.path(), &github);
    for words in [
        &["token", "sam", "--bare"][..],
        &["token", "--bare"],
        &["token", "sam/r", "eve/r", "--bare"],
        &["token", "sam/r", "--bare", "--bare"],
        &["token", "sam/../r", "--bare"],
    ] {
        let out = upkeep(dir.path(), words);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{words:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(text(&out.stdout), "");
    }
    let out = upkeep(dir.path(), &["token", "sam/r"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        text(&out.stderr).contains("isn't an owner"),
        "{}",
        text(&out.stderr)
    );
    assert!(github.asked().is_empty(), "{:?}", github.asked());
}

/// Every file under `dir`.
fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut all = vec![];
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            all.extend(walk(&p));
        } else {
            all.push(p);
        }
    }
    all
}
