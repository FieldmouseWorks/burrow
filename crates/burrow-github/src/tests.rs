use super::fake::{Fake, ok};
use super::*;
use serde_json::json;

#[test]
fn encodes_a_query_value() {
    assert_eq!(encode("updates"), "updates");
    assert_eq!(encode("needs peter&co"), "needs%20peter%26co");
    assert_eq!(encode("é"), "%C3%A9");
    assert_eq!(encode("repo:a/b \"x\""), "repo%3Aa%2Fb%20%22x%22");
}

#[test]
fn reads_gh() {
    let r = parse("HTTP/2.0 200 OK\r\nEtag: W/\"abc\"\r\nLink: <https://api.github.com/repositories/1/issues?page=2>; rel=\"next\", <https://api.github.com/repositories/1/issues?page=3>; rel=\"last\"\r\n\r\n[1]").unwrap();
    assert_eq!(
        (r.status, r.header("ETAG"), r.body.as_str()),
        (200, Some("W/\"abc\""), "[1]")
    );
    assert_eq!(r.next().as_deref(), Some("repositories/1/issues?page=2"));
    let r = parse("HTTP/2.0 304 Not Modified\nEtag: x\n").unwrap();
    assert_eq!((r.status, r.body.as_str(), r.next()), (304, "", None));
    assert_eq!(r.header("etag"), Some("x"));
    // What gh really prints: \n after the status, \r\n after headers, and
    // a body that can hold either blank line itself.
    let r = parse(
        "HTTP/1.1 200 OK\nEtag: \"e\"\r\nServer: github.com\r\n\r\n[\"a\\n\\nb\", \"\r\n\r\n\"]",
    )
    .unwrap();
    assert_eq!(
        (r.header("etag"), r.header("server"), r.body.as_str()),
        (
            Some("\"e\""),
            Some("github.com"),
            "[\"a\\n\\nb\", \"\r\n\r\n\"]"
        )
    );
    assert!(parse("").is_none());
    assert!(parse("error connecting to api.github.com").is_none());
}

#[test]
fn tries_again_on_what_passes() {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "a",
        vec![
            Err(Failed {
                why: "gh: error connecting".into(),
                transient: true,
            }),
            ok(502, Value::Null, &[]),
            ok(200, json!([1]), &[]),
        ],
    );
    assert_eq!(call(&mut gh, "GET", "a", None, None).unwrap().body, "[1]");
    assert_eq!(gh.paused, vec![2, 8]);

    let mut gh = Fake::default();
    gh.on("GET", "a", vec![ok(503, Value::Null, &[])]);
    assert_eq!(
        call(&mut gh, "GET", "a", None, None).unwrap_err().why,
        "GET a: HTTP 503, 3 times"
    );
    assert_eq!(gh.asked.len(), 3);
}

#[test]
fn answers_what_the_caller_reads() {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "c",
        vec![ok(404, json!({"message": "Not Found"}), &[])],
    );
    let r = call_allowing(&mut gh, "GET", "c", None, None, &[404]).unwrap();
    assert_eq!((r.status, gh.asked.len()), (404, 1));
    // Only those: another refusal is still an Error, and so is the same
    // one to a plain call.
    assert_eq!(
        call(&mut gh, "GET", "c", None, None).unwrap_err().why,
        "GET c: HTTP 404: Not Found"
    );
    assert!(call_allowing(&mut gh, "GET", "c", None, None, &[422]).is_err());
    // What's worth asking again still is.
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "c",
        vec![ok(502, Value::Null, &[]), ok(404, json!({}), &[])],
    );
    let r = call_allowing(&mut gh, "GET", "c", None, None, &[404]).unwrap();
    assert_eq!((r.status, gh.paused.as_slice()), (404, &[2][..]));
}

/// A GitHub that keeps whose account each GraphQL call was about.
struct Graph {
    asked: Vec<(String, Value)>,
    answers: Vec<Result<Response, Failed>>,
}

impl Api for Graph {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        _: Option<&Value>,
        _: Option<&str>,
    ) -> Result<Response, Failed> {
        panic!("{method} {path} isn't GraphQL")
    }

    fn graphql(&mut self, owner: &str, body: &Value) -> Result<Response, Failed> {
        self.asked.push((owner.into(), body.clone()));
        self.answers.remove(0)
    }

    fn pause(&mut self, _: u64) {}
}

#[test]
fn queries_as_the_account_they_are_about() {
    let mut gh = Graph {
        asked: vec![],
        answers: vec![
            ok(502, Value::Null, &[]),
            ok(200, json!({"data": {"viewer": {"login": "sam"}}}), &[]),
        ],
    };
    let data = query(
        &mut gh,
        "Sam",
        "query { viewer { login } }",
        json!({"a": 1}),
    )
    .unwrap();
    assert_eq!(data, json!({"viewer": {"login": "sam"}}));
    assert_eq!(gh.asked.len(), 2);
    assert_eq!(
        gh.asked[1],
        (
            "Sam".into(),
            json!({"query": "query { viewer { login } }", "variables": {"a": 1}})
        )
    );
    // An answer that holds errors is an Error, whatever its status.
    let mut gh = Graph {
        asked: vec![],
        answers: vec![ok(
            200,
            json!({"data": {"repositoryOwner": null}, "errors": [
                {"message": "Could not resolve to a RepositoryOwner with the login of 'nobody'."},
                {"message": "and more"}
            ]}),
            &[],
        )],
    };
    assert_eq!(
        query(&mut gh, "nobody", "q", json!({})).unwrap_err().why,
        "POST graphql: Could not resolve to a RepositoryOwner with the login of 'nobody'.; and more"
    );
    let mut gh = Graph {
        asked: vec![],
        answers: vec![ok(401, json!({"message": "Bad credentials"}), &[])],
    };
    assert_eq!(
        query(&mut gh, "sam", "q", json!({})).unwrap_err().why,
        "POST graphql: HTTP 401: Bad credentials"
    );
    // Asked of a GitHub with nothing of its own for it, it's a POST.
    let mut gh = Fake::default();
    gh.on("POST", "graphql", vec![ok(200, json!({"data": 1}), &[])]);
    assert_eq!(query(&mut gh, "sam", "q", json!({})).unwrap(), json!(1));
    assert_eq!(gh.asked[0].1, "graphql");
}

#[test]
fn writes_once_what_a_replay_could_undo() {
    // A 502 can come after GitHub took the PATCH: asked again, it would
    // write over an edit made meanwhile.
    let mut gh = Fake::default();
    gh.on(
        "PATCH",
        "repos/sam/r/pulls/7",
        vec![ok(502, Value::Null, &[])],
    );
    let body = json!({"body": "new"});
    let e = call_once(&mut gh, "PATCH", "repos/sam/r/pulls/7", Some(&body)).unwrap_err();
    assert!(
        e.unanswered && e.why.starts_with("PATCH repos/sam/r/pulls/7: HTTP 502"),
        "{}",
        e.why
    );
    assert_eq!(gh.asked("PATCH").len(), 1);
    // Nor after a 429: GitHub did nothing, but the edit it would undo
    // can come while it waits.
    let mut gh = Fake::default();
    gh.on(
        "PATCH",
        "repos/sam/r/pulls/7",
        vec![ok(429, Value::Null, &[]), ok(200, json!({}), &[])],
    );
    let e = call_once(&mut gh, "PATCH", "repos/sam/r/pulls/7", Some(&body)).unwrap_err();
    assert!(e.unanswered, "{}", e.why);
    assert_eq!(gh.asked("PATCH").len(), 1);
    // Its answer, when it has one.
    let mut gh = Fake::default();
    gh.on(
        "PATCH",
        "repos/sam/r/pulls/7",
        vec![ok(200, json!({}), &[])],
    );
    assert!(call_once(&mut gh, "PATCH", "repos/sam/r/pulls/7", Some(&body)).is_ok());
}

#[test]
fn asks_a_mutation_again_only_when_github_did_nothing() {
    // A 502 can come after GitHub posted the reply: asked again, it
    // would post a second.
    let mut gh = Graph {
        asked: vec![],
        answers: vec![ok(502, Value::Null, &[])],
    };
    let e = mutate(&mut gh, "sam", "mutation { m }", json!({})).unwrap_err();
    assert_eq!(
        (e.why.as_str(), e.unanswered),
        ("POST graphql: HTTP 502: ", true)
    );
    assert_eq!(gh.asked.len(), 1);
    // A 429 is GitHub doing nothing: asked again, as the account it's about.
    let mut gh = Graph {
        asked: vec![],
        answers: vec![
            ok(429, Value::Null, &[]),
            ok(200, json!({"data": {"m": 2}}), &[]),
        ],
    };
    assert_eq!(
        mutate(&mut gh, "Sam", "mutation { m }", json!({"t": "x"})).unwrap(),
        json!({"m": 2})
    );
    assert_eq!(
        gh.asked,
        vec![
            (
                "Sam".into(),
                json!({"query": "mutation { m }", "variables": {"t": "x"}})
            );
            2
        ]
    );
    // Its errors are an Error, as a query's are.
    let mut gh = Graph {
        asked: vec![],
        answers: vec![ok(
            200,
            json!({"data": null, "errors": [{"message": "no"}]}),
            &[],
        )],
    };
    assert_eq!(
        mutate(&mut gh, "sam", "m", json!({})).unwrap_err().why,
        "POST graphql: no"
    );
}

#[test]
fn asks_a_post_again_only_when_github_did_nothing() {
    // GitHub can have made the issue when its answer is lost or a 5xx:
    // asking again would make a second.
    for (answer, said) in [
        (
            Err(Failed {
                why: "gh: connection reset by peer".into(),
                transient: true,
            }),
            "POST b: gh: connection reset by peer",
        ),
        (ok(502, Value::Null, &[]), "POST b: HTTP 502: "),
    ] {
        let mut gh = Fake::default();
        gh.on("POST", "b", vec![answer]);
        let e = call(&mut gh, "POST", "b", Some(&json!({})), None).unwrap_err();
        assert_eq!(e.why, said);
        assert_eq!(gh.asked.len(), 1);
        assert!(gh.paused.is_empty());
    }
    // A 429 is refused before anything's done.
    let mut gh = Fake::default();
    gh.on(
        "POST",
        "b",
        vec![
            ok(429, Value::Null, &[]),
            ok(201, json!({"number": 7}), &[]),
        ],
    );
    let r = call(&mut gh, "POST", "b", Some(&json!({})), None).unwrap();
    assert_eq!(r.status, 201);
    assert_eq!(gh.paused, vec![2]);
    // What asking again can't do twice still is.
    for method in ["PATCH", "PUT", "DELETE"] {
        let mut gh = Fake::default();
        gh.on(
            method,
            "c",
            vec![ok(502, Value::Null, &[]), ok(200, json!({}), &[])],
        );
        assert_eq!(call(&mut gh, method, "c", None, None).unwrap().status, 200);
    }
}

#[test]
fn asks_a_write_that_comes_out_the_same_again() {
    let mut gh = Fake::default();
    gh.on(
        "POST",
        "s",
        vec![
            Err(Failed {
                why: "gh: connection reset by peer".into(),
                transient: true,
            }),
            ok(502, Value::Null, &[]),
            ok(201, json!({}), &[]),
        ],
    );
    let r = call_repeating(&mut gh, "POST", "s", Some(&json!({}))).unwrap();
    assert_eq!(r.status, 201);
    assert_eq!(gh.paused, vec![2, 8]);
    // Not past the tries a GET gets, and GitHub's no is final.
    let mut gh = Fake::default();
    gh.on("POST", "s", vec![ok(502, Value::Null, &[])]);
    let e = call_repeating(&mut gh, "POST", "s", None).unwrap_err();
    assert_eq!(
        (e.why.as_str(), e.unanswered),
        ("POST s: HTTP 502, 3 times", true)
    );
    let mut gh = Fake::default();
    gh.on(
        "POST",
        "s",
        vec![ok(422, json!({"message": "No commit"}), &[])],
    );
    let e = call_repeating(&mut gh, "POST", "s", None).unwrap_err();
    assert_eq!(e.why, "POST s: HTTP 422: No commit");
    assert_eq!(gh.asked.len(), 1);
}

#[test]
fn gives_up_on_what_wont() {
    let mut gh = Fake::default();
    gh.on(
        "POST",
        "b",
        vec![ok(
            403,
            json!({"message": "Resource not accessible by integration"}),
            &[],
        )],
    );
    let e = call(&mut gh, "POST", "b", Some(&json!({})), None).unwrap_err();
    assert_eq!(
        e.why,
        "POST b: HTTP 403: Resource not accessible by integration"
    );
    assert_eq!((gh.asked.len(), gh.paused.len()), (1, 0));

    let mut gh = Fake::default();
    gh.on(
        "GET",
        "b",
        vec![Err(Failed {
            why: "gh: To get started with GitHub CLI, please run: gh auth login".into(),
            transient: false,
        })],
    );
    assert!(
        call(&mut gh, "GET", "b", None, None)
            .unwrap_err()
            .why
            .contains("gh auth login")
    );
    assert_eq!(gh.asked.len(), 1);
    assert!(
        Failed::from_gh("dial tcp: i/o timeout".into())
            .unwrap_err()
            .transient
    );
    assert!(
        !Failed::from_gh("HTTP 401: Bad credentials".into())
            .unwrap_err()
            .transient
    );
    // The URL gh quotes is the call's, not what went wrong.
    assert!(
        !Failed::from_gh(
            "Get \"https://api.github.com/repos/sam/timeout/git/ref/eof\": invalid header".into()
        )
        .unwrap_err()
        .transient
    );
}

#[test]
fn says_whether_github_answered_by_the_status_never_the_words() {
    // Every answer names the repo `timeout` and the branch `eof`, as a
    // path and as GitHub's message: neither makes an outage.
    let path = "repos/sam/timeout/compare/main...eof";
    let message = json!({"message": "No common ancestor between main and eof: timeout"});
    for (answer, method, unanswered) in [
        (ok(404, message.clone(), &[]), "GET", false),
        (ok(422, message.clone(), &[]), "POST", false),
        (ok(403, message.clone(), &[]), "GET", false),
        (ok(503, message.clone(), &[]), "GET", true),
        (ok(502, message.clone(), &[]), "POST", true),
        (ok(429, message.clone(), &[]), "GET", true),
        (
            ok(403, message.clone(), &[("x-ratelimit-remaining", "0")]),
            "GET",
            true,
        ),
        (
            ok(403, message.clone(), &[("retry-after", "60")]),
            "GET",
            true,
        ),
        (
            ok(
                403,
                json!({"message": "You have exceeded a secondary rate limit"}),
                &[],
            ),
            "GET",
            true,
        ),
        (
            Err(Failed {
                why: "gh: connection reset by peer".into(),
                transient: true,
            }),
            "GET",
            true,
        ),
        (
            Err(Failed {
                why: "gh: connection reset by peer".into(),
                transient: true,
            }),
            "POST",
            true,
        ),
        (
            Err(Failed {
                why: format!("gh: Get \"https://api.github.com/{path}\": bad"),
                transient: false,
            }),
            "GET",
            false,
        ),
    ] {
        let mut gh = Fake::default();
        gh.on(method, path, vec![answer]);
        let e = call(&mut gh, method, path, None, None).unwrap_err();
        assert_eq!(e.unanswered, unanswered, "{e}");
    }
    // An answer that won't do is GitHub's.
    let mut gh = Fake::default();
    gh.on("GET", path, vec![ok(200, Value::Null, &[])]);
    let r = call(&mut gh, "GET", path, None, None).unwrap();
    assert!(!r.json().unwrap_err().unanswered);
}

/// A Gh whose gh is run under a stand-in for runuser, which answers as
/// gh would, with what it was run as: its words, and the environment's
/// `shows`.
fn under(shows: &str) -> Gh {
    let answer =
        format!(r#"cat >/dev/null; printf 'HTTP/1.1 200 OK\n\n%s|%s' "$*" "${{{shows}-}}""#);
    Gh {
        program: None,
        token: None,
        via: ["/bin/sh", "-c", &answer, "runuser"]
            .map(OsString::from)
            .to_vec(),
    }
}

#[test]
fn runs_gh_under_its_via() {
    let mut gh = under("GH_TOKEN_OWNER");
    let r = gh.request("GET", "repos/sam/homelab", None, None).unwrap();
    assert_eq!(r.body, "gh api -i -X GET repos/sam/homelab|");
    // GraphQL as the account it's about, through whatever runs gh.
    let r = gh.graphql("sam", &json!({"query": "q"})).unwrap();
    assert_eq!(r.body, "gh api -i -X POST graphql --input -|sam");
    let mut gh = Gh {
        token: Some("ghs_x".into()),
        ..under("GH_TOKEN")
    };
    let r = gh.request("GET", "a", None, Some("e")).unwrap();
    assert_eq!(r.body, "gh api -i -X GET a -H If-None-Match: e|ghs_x");
}
