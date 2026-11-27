//! Races with concurrent merges and branch deletions: the default branch
//! moving while a pass reads it, a house branch vanishing between POST and
//! PATCH, and POST refusals or rate limits that PATCH must never mask.

use std::collections::VecDeque;
use std::fs;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use burrow_github::{Api, Failed, Response};
use house_sync::pass::{self, Ask};
use serde_json::{Value, json};

use super::github::{Asked, Notices, answer, limited};
use super::{ALL_CLEAR, Setup, branch, config, current, setup, stale};

const SOON: u64 = 4_000_000_000;

struct RaceGitHub {
    /// `(sha, file_sha, agents_text)` for `main` before and after a concurrent
    /// move. When `moved_after_agents` is true, `main` switches to `after` as
    /// soon as `AGENTS.md` is read at `before`.
    before: (&'static str, &'static str, String),
    after: Option<(&'static str, &'static str, String)>,
    moved: bool,
    /// Queued answers for `POST repos/sam/a/git/refs`. Falls back to 201.
    post_ref: VecDeque<Result<Response, Failed>>,
    /// Queued answers for `PATCH repos/sam/a/git/refs/heads/...`. Falls back to 200.
    patch_ref: VecDeque<Result<Response, Failed>>,
    asked: Vec<Asked>,
}

impl RaceGitHub {
    fn new(agents: String) -> Self {
        Self {
            before: ("c0ffee", "f11e", agents),
            after: None,
            moved: false,
            post_ref: VecDeque::new(),
            patch_ref: VecDeque::new(),
            asked: Vec::new(),
        }
    }

    fn current_tip(&self) -> (&'static str, &'static str, &str) {
        if self.moved
            && let Some((sha, fsha, text)) = &self.after
        {
            return (sha, fsha, text);
        }
        (self.before.0, self.before.1, &self.before.2)
    }

    fn agents_for(&self, at: &str) -> (&'static str, &str) {
        if let Some((sha, fsha, text)) = &self.after
            && *sha == at
        {
            return (fsha, text);
        }
        (self.before.1, &self.before.2)
    }
}

impl Api for RaceGitHub {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        _: Option<&str>,
    ) -> Result<Response, Failed> {
        self.asked.push((method.into(), path.into(), body.cloned()));
        let (bare, query) = path.split_once('?').unwrap_or((path, ""));
        let parts: Vec<&str> = bare.split('/').collect();
        match (method, &parts[..]) {
            ("GET", ["repos", "sam", "a"]) => answer(
                200,
                json!({
                    "full_name": "sam/a",
                    "default_branch": "main",
                    "owner": {"login": "Sam"},
                    "archived": false,
                    "fork": false,
                }),
                None,
            ),
            ("GET", ["repos", "sam", "a", "git", "ref", "heads", "main"]) => {
                let (sha, _, _) = self.current_tip();
                answer(200, json!({"object": {"sha": sha}}), None)
            }
            ("GET", ["repos", "sam", "a", "contents", "AGENTS.md"]) => {
                let at = query.strip_prefix("ref=").unwrap_or(self.before.0);
                let (fsha, text) = self.agents_for(at);
                let resp = answer(
                    200,
                    json!({"sha": fsha, "content": STANDARD.encode(text)}),
                    None,
                );
                if self.after.is_some() && !self.moved {
                    self.moved = true;
                }
                resp
            }
            ("GET", ["repos", "sam", "a", "contents", "house", _]) => {
                answer(404, json!({"message": "Not Found"}), None)
            }
            ("GET", ["repos", "sam", "a", "pulls"]) => answer(200, json!([]), None),
            ("POST", ["repos", "sam", "a", "git", "refs"]) => self
                .post_ref
                .pop_front()
                .unwrap_or_else(|| answer(201, json!({}), None)),
            ("PATCH", ["repos", "sam", "a", "git", "refs", "heads", ..]) => self
                .patch_ref
                .pop_front()
                .unwrap_or_else(|| answer(200, json!({}), None)),
            ("PUT", ["repos", "sam", "a", "contents", "AGENTS.md"]) => answer(200, json!({}), None),
            ("POST", ["repos", "sam", "a", "pulls"]) => answer(
                201,
                json!({"html_url": "https://github.com/sam/a/pull/9"}),
                None,
            ),
            _ => panic!("unexpected call: {method} {path}"),
        }
    }

    fn graphql(&mut self, _: &str, _: &Value) -> Result<Response, Failed> {
        answer(
            200,
            json!({"data": {"repositoryOwner": {"repositories": {
                "nodes": [{
                    "nameWithOwner": "sam/a",
                    "isArchived": false,
                    "defaultBranchRef": {"name": "main"},
                }],
                "pageInfo": {"hasNextPage": false, "endCursor": Value::Null},
            }}}}),
            None,
        )
    }

    fn pause(&mut self, _: u64) {}
}

struct RaceRan {
    ok: Result<bool, String>,
    asked: Vec<Asked>,
    told: Vec<Vec<String>>,
    out: String,
}

fn run_race(api: &mut RaceGitHub, only: Option<&str>, s: &Setup) -> RaceRan {
    let mut notices = Notices::default();
    let mut out = Vec::new();
    let ok = pass::run(
        &config(s),
        &Ask { dry: false, only },
        api,
        &mut notices,
        &mut out,
    );
    RaceRan {
        ok,
        asked: api.asked.clone(),
        told: notices.told,
        out: String::from_utf8(out).unwrap(),
    }
}

#[test]
fn a_house_pr_merged_during_the_pass_neither_fails_nor_opens_a_duplicate() {
    // Subcase A: the house PR merges between head()/agents() and open_prs(),
    // and its head branch is deleted between POST (422 Reference already
    // exists) and PATCH (422 Reference does not exist).
    // Subcase B: the merged house PR's branch is already gone before POST.
    // Neither should fail or open a duplicate PR from the stale commit.
    for branch_deleting in [true, false] {
        let mut api = RaceGitHub::new(stale());
        api.after = Some(("decaf", "f22e", current()));
        if branch_deleting {
            api.post_ref.push_back(answer(
                422,
                json!({"message": "Reference already exists"}),
                None,
            ));
            api.patch_ref.push_back(answer(
                422,
                json!({"message": "Reference does not exist"}),
                None,
            ));
        }
        let ran = run_race(&mut api, None, &setup());
        assert_eq!(
            ran.ok,
            Ok(true),
            "branch_deleting={branch_deleting}: {}",
            ran.out
        );
        let notices: Vec<[&str; 2]> = ran
            .told
            .iter()
            .map(|t| [t[0].as_str(), t[1].as_str()])
            .collect();
        assert_eq!(notices, ALL_CLEAR, "branch_deleting={branch_deleting}");
        assert!(
            !ran.asked
                .iter()
                .any(|(m, _, _)| matches!(m.as_str(), "POST" | "PATCH" | "PUT")),
            "must not mutate after main moved to current blocks: {:?}",
            ran.asked
        );
    }

    // Subcase C: an unrelated PR merges to main between head()/agents() and
    // open_prs(), leaving AGENTS.md stale at the new tip `decaf`. The house
    // branch must start from `decaf`, not stale `c0ffee`.
    let mut api = RaceGitHub::new(stale());
    api.after = Some(("decaf", "f22e", stale()));
    let ran = run_race(&mut api, Some("sam/a"), &setup());
    assert_eq!(ran.ok, Ok(true), "{}", ran.out);
    let post_ref = ran
        .asked
        .iter()
        .find(|(m, p, _)| m == "POST" && p == "repos/sam/a/git/refs")
        .expect("creates branch");
    assert_eq!(
        post_ref.2,
        Some(json!({"ref": format!("refs/heads/{}", branch()), "sha": "decaf"}))
    );
    let put = ran
        .asked
        .iter()
        .find(|(m, p, _)| m == "PUT" && p == "repos/sam/a/contents/AGENTS.md")
        .expect("writes AGENTS.md");
    assert_eq!(put.2.as_ref().unwrap()["sha"], json!("f22e"));
}

#[test]
fn a_leftover_branch_deleted_between_post_and_patch_is_retried_and_bounded() {
    // Leftover branch exists on POST, disappears before PATCH, while main is
    // still stale: retrying re-reads and creates the branch cleanly on attempt 2.
    let mut api = RaceGitHub::new(stale());
    api.post_ref.push_back(answer(
        422,
        json!({"message": "Reference already exists"}),
        None,
    ));
    api.patch_ref.push_back(answer(
        422,
        json!({"message": "Reference does not exist"}),
        None,
    ));
    let ran = run_race(&mut api, Some("sam/a"), &setup());
    assert_eq!(ran.ok, Ok(true), "{}", ran.out);
    assert_eq!(
        ran.asked
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == "repos/sam/a/git/refs")
            .count(),
        2
    );
    assert!(
        ran.asked
            .iter()
            .any(|(m, p, _)| m == "POST" && p == "repos/sam/a/pulls")
    );

    // If the branch keeps vanishing on every attempt, the retry loop is
    // bounded and fails the repo with the PATCH error instead of looping.
    let mut stuck = RaceGitHub::new(stale());
    for _ in 0..5 {
        stuck.post_ref.push_back(answer(
            422,
            json!({"message": "Reference already exists"}),
            None,
        ));
        stuck.patch_ref.push_back(answer(
            422,
            json!({"message": "Reference does not exist"}),
            None,
        ));
    }
    let ran = run_race(&mut stuck, Some("sam/a"), &setup());
    assert_eq!(ran.ok, Ok(false), "{}", ran.out);
    assert_eq!(
        ran.asked
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == "repos/sam/a/git/refs")
            .count(),
        3
    );
    assert!(
        ran.out.contains("PATCH repos/sam/a/git/refs/heads/")
            && ran.out.contains("Reference does not exist"),
        "{}",
        ran.out
    );
}

#[test]
fn post_ref_refusal_or_rate_limit_is_not_masked_by_patch() {
    // Rate limit on POST git/refs: booked as a rate limit, never falls through
    // to PATCH.
    let dir = tempfile::tempdir().unwrap();
    let s = Setup {
        wake: dir.path().join("wake.json"),
        ..setup()
    };
    let mut api = RaceGitHub::new(stale());
    api.post_ref.push_back(limited(SOON));
    let ran = run_race(&mut api, Some("sam/a"), &s);
    assert_eq!(ran.ok, Ok(true), "{}", ran.out);
    assert!(
        !ran.asked.iter().any(|(m, _, _)| m == "PATCH"),
        "rate limit on POST must not trigger PATCH: {:?}",
        ran.asked
    );
    let wake: Value = serde_json::from_slice(&fs::read(&s.wake).unwrap()).unwrap();
    assert_eq!(wake, json!({"sam/a": SOON}));

    // Non-duplicate refusal on POST git/refs (403 or another 422): fails with
    // the POST error, never calls PATCH.
    for (status, msg) in [
        (403, "Resource not accessible by integration"),
        (422, "Validation Failed"),
    ] {
        let mut api = RaceGitHub::new(stale());
        api.post_ref
            .push_back(answer(status, json!({"message": msg}), None));
        let ran = run_race(&mut api, Some("sam/a"), &setup());
        assert_eq!(ran.ok, Ok(false), "{status} {msg}: {}", ran.out);
        assert!(
            !ran.asked.iter().any(|(m, _, _)| m == "PATCH"),
            "{status} {msg}: must not call PATCH: {:?}",
            ran.asked
        );
        assert!(
            ran.out.contains("POST repos/sam/a/git/refs") && ran.out.contains(msg),
            "{}",
            ran.out
        );
    }

    // A 422 refusal on PATCH other than "Reference does not exist" is not a
    // retryable race: fail immediately without retrying.
    let mut api = RaceGitHub::new(stale());
    api.post_ref.push_back(answer(
        422,
        json!({"message": "Reference already exists"}),
        None,
    ));
    api.patch_ref.push_back(answer(
        422,
        json!({"message": "Update is not a fast forward"}),
        None,
    ));
    let ran = run_race(&mut api, Some("sam/a"), &setup());
    assert_eq!(ran.ok, Ok(false), "{}", ran.out);
    assert_eq!(
        ran.asked
            .iter()
            .filter(|(m, p, _)| m == "POST" && p == "repos/sam/a/git/refs")
            .count(),
        1
    );
    assert!(
        ran.out.contains("Update is not a fast forward"),
        "{}",
        ran.out
    );
}
