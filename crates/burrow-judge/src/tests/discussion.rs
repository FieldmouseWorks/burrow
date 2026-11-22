//! The discussion as the judge reads it from GitHub itself, through
//! Conary's own `Api`: the digest it takes must be the one the flow takes
//! of the same threads and reviews, App authors included.
use super::{PR, REPO};
use crate::discussion::{self, Discussion};
use conary_github::api::Review;
use conary_github::flow::{ReviewThread, ThreadComment, judging_context, review_bodies};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::TcpListener;
use std::thread;

const CONNECTOR: &str = "chatgpt-codex-connector";

/// The PR's snapshot as GitHub's GraphQL answers it: one review thread
/// whose finding is the review connector's, which GraphQL names by its
/// slug alone and types a `Bot`, and a person's answer.
fn snapshot() -> Value {
    let mut v = json!({"data":{"repository":{"defaultBranchRef":{"name":"main","target":{"oid":"base-tip","statusCheckRollup":{"state":"SUCCESS"}}},"pullRequest":{
        "headRefOid":"h","baseRefName":"main","title":"t","body":"b","isDraft":false,"mergeStateStatus":"CLEAN","updatedAt":"2026-11-08T12:00:00Z",
        "lastEditedAt":null,
        "timelineItems":{"pageInfo":{"hasPreviousPage":false},"nodes":[]},
        "retargets":{"nodes":[]},
        "labels":{"pageInfo":{"hasNextPage":false}},
        "reviewThreads":{"pageInfo":{"hasNextPage":false},"nodes":[
            {"id":"PRRT_1","isResolved":true,"path":"src/lib.rs","line":12,"originalLine":12,"comments":{"pageInfo":{"hasNextPage":false},"nodes":[
                {"author":{"__typename":"Bot","login":CONNECTOR},"body":"P2: the loop never ends","pullRequestReview":{"databaseId":7}},
                {"author":{"__typename":"User","login":"sam"},"body":"It ends on the empty line.","pullRequestReview":null}]}}]},
        "reviews":{"totalCount":1},"comments":{"totalCount":0},
        "commits":{"nodes":[{"commit":{"oid":"h","checkSuites":{"pageInfo":{"hasNextPage":false},"nodes":[]}}}]}
    }}}});
    // Kept apart: one more field nests the literal past `json!`'s limit.
    v["data"]["repository"]["pullRequest"]["renames"] = json!({"nodes": []});
    v
}

/// The PR's reviews as REST answers them: REST names an App `NAME[bot]`.
fn reviews() -> Value {
    json!([{"id": 7, "state": "COMMENTED", "body": "One finding.",
            "user": {"login": format!("{CONNECTOR}[bot]"), "type": "Bot"},
            "commit_id": "h", "html_url": null, "submitted_at": null}])
}

/// A loopback GitHub answering every read the discussion makes, as often
/// as it's asked.
fn github() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap();
            let mut length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                if line == "\r\n" || line.is_empty() {
                    break;
                }
                if let Some(n) = line.to_lowercase().strip_prefix("content-length: ") {
                    length = n.trim().parse().unwrap();
                }
            }
            reader.read_exact(&mut vec![0; length]).unwrap();
            let body = if request.contains("/graphql") {
                snapshot()
            } else if request.contains("/reviews") && request.contains("page=1") {
                reviews()
            } else {
                json!([])
            }
            .to_string();
            let _ = write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    base
}

#[test]
fn an_apps_thread_comment_is_read_by_its_rest_login_and_digested_as_the_flow_does() {
    let api = conary_github::Api::new(&github(), conary_github::Tokens::given("t"));
    let read = Discussion::read(&api, REPO, PR).unwrap();
    let authors: Vec<_> = read.threads["PRRT_1"]
        .comments
        .iter()
        .map(|c| c.author.as_deref())
        .collect();
    assert_eq!(
        authors,
        [Some("chatgpt-codex-connector[bot]"), Some("sam")],
        "GraphQL's Bot is read by the login REST gives it"
    );
    // The flow keeps every author by REST's login: the same thread and
    // review, as it holds them, digest to what the judge read.
    let comment = |author: &str, body: &str| ThreadComment {
        author: Some(author.into()),
        body: body.into(),
    };
    let flows = BTreeMap::from([(
        "PRRT_1".to_owned(),
        ReviewThread {
            resolved: true,
            path: "src/lib.rs".into(),
            line: Some(12),
            comments: vec![
                comment("chatgpt-codex-connector[bot]", "P2: the loop never ends"),
                comment("sam", "It ends on the empty line."),
            ],
        },
    )]);
    let rest: Vec<Review> = serde_json::from_value(reviews()).unwrap();
    assert_eq!(
        read.context(),
        judging_context(&flows, &review_bodies(&rest))
    );
    // And the producer's own check reads the same digest.
    assert_eq!(discussion::context(&api, REPO, PR).unwrap(), read.context());
}
