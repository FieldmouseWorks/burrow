//! Text and its edit history are read together before crossing the job boundary.
use super::{Pass, raw};
use serde_json::{Value, json};

const QUERY: &str = "query($owner:String!,$name:String!,$n:Int!,$cursor:String){repository(owner:$owner,name:$name){issue(number:$n){author{login __typename} title body userContentEdits(first:100){totalCount nodes{editor{login __typename} deletedBy{login __typename}}} timelineItems(itemTypes:[RENAMED_TITLE_EVENT],first:100){totalCount nodes{... on RenamedTitleEvent{actor{login __typename}}}} comments(first:100,after:$cursor){pageInfo{hasNextPage endCursor} totalCount nodes{databaseId author{login __typename} body createdAt userContentEdits(first:100){totalCount nodes{editor{login __typename} deletedBy{login __typename}}}}}}}}";
/// Verified text plus the original snapshot, retained only for refresh comparisons.
pub(super) struct Written {
    pub issue: Value,
    pub comments: Vec<Value>,
    original: [Value; 2],
}
impl Written {
    pub fn matches(&self, issue: &Value) -> bool {
        self.original
            .iter()
            .zip([&issue["title"], &issue["body"]])
            .all(|(a, b)| a.as_str().unwrap_or_default() == b.as_str().unwrap_or_default())
    }
}

impl Pass<'_> {
    fn writer(&self, actor: &Value) -> bool {
        actor["login"].as_str().is_some_and(|login| {
            let login = if actor["__typename"] == "Bot" && !login.ends_with("[bot]") {
                format!("{login}[bot]")
            } else {
                login.to_owned()
            };
            self.allowed(&login)
        })
    }
    fn writers(&self, list: &Value, field: &str) -> bool {
        list["nodes"].as_array().is_some_and(|nodes| {
            // GitHub prunes intermediate body edits once 100 are retained.
            // Equality at that limit no longer proves that every editor is known.
            nodes.len() < 100
                && list["totalCount"].as_u64() == Some(nodes.len() as u64)
                && nodes.iter().all(|e| {
                    self.writer(&e[field])
                        && (e["deletedBy"].is_null() || self.writer(&e["deletedBy"]))
                })
        })
    }
    fn authored(&self, text: &Value) -> bool {
        self.writer(&text["author"]) && self.writers(&text["userContentEdits"], "editor")
    }
    pub(super) fn written(&self, repo: &str, rest: &Value) -> Option<Written> {
        let (owner, name) = repo.split_once('/')?;
        let mut cursor = None;
        let mut comments = Vec::new();
        loop {
            let mut args = vec![
                "graphql".to_owned(),
                "-f".into(),
                format!("query={QUERY}"),
                "-f".into(),
                format!("owner={owner}"),
                "-f".into(),
                format!("name={name}"),
                "-F".into(),
                format!("n={}", raw(&rest["number"])),
            ];
            if let Some(c) = &cursor {
                args.extend(["-f".into(), format!("cursor={c}")]);
            }
            let got = crate::github::api(
                self.cfg,
                &args.iter().map(String::as_str).collect::<Vec<_>>(),
            )?;
            if got.get("errors").is_some() {
                return None;
            }
            let mut issue = got["data"]["repository"]["issue"].clone();
            let page = &issue["comments"];
            comments.extend(page["nodes"].as_array()?.iter().cloned());
            if page["pageInfo"]["hasNextPage"] == true {
                let next = page["pageInfo"]["endCursor"].as_str()?.to_owned();
                if cursor.as_ref() == Some(&next) {
                    return None;
                }
                cursor = Some(next);
                continue;
            }
            issue["comments"]["nodes"] = json!(comments);
            return self.checked(rest, &issue);
        }
    }

    fn checked(&self, rest: &Value, issue: &Value) -> Option<Written> {
        let nodes = issue["comments"]["nodes"].as_array()?;
        // Fail closed if pagination did not establish the complete history.
        if issue["comments"]["totalCount"].as_u64() != Some(nodes.len() as u64) {
            return None;
        }
        let mut safe = rest.clone();
        if self.authored(issue) && self.writers(&issue["timelineItems"], "actor") {
            safe["title"] = issue["title"].clone();
            safe["body"] = issue["body"].clone();
        } else {
            safe["title"] = json!("Issue text withheld");
            safe["body"] = json!(
                "The issue's title or body has an untrusted or unknown writer. Ask an authorized author for a new issue."
            );
        }
        let comments = nodes.iter().filter(|c| self.authored(c)).map(|c| json!({"id":c["databaseId"], "user":{"login": if c["author"]["__typename"] == "Bot" { json!(format!("{}[bot]", c["author"]["login"].as_str().unwrap_or_default().trim_end_matches("[bot]"))) } else { c["author"]["login"].clone() }}, "body":c["body"], "created_at":c["createdAt"]})).collect();
        Some(Written {
            issue: safe,
            comments,
            original: [issue["title"].clone(), issue["body"].clone()],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::sorting_tests::{Box, issue};
    fn text() -> Value {
        let edits = json!({"totalCount":0,"nodes":[]});
        json!({"author":{"login":"sam"},"title":"trusted title","body":"trusted body", "userContentEdits":edits,"timelineItems":edits,"comments":{"totalCount":1,"nodes":[{"databaseId":9,"author":{"login":"sam"},"body":"trusted reply","createdAt":"2026-11-01T00:00:00Z","userContentEdits":edits}]}})
    }
    #[test]
    fn outside_edits_never_reach_a_job() {
        let b = Box::new();
        let rest = issue(7, "sam", &["claude"]);
        let bad = json!({"totalCount":1,"nodes":[{"editor":{"login":"mallory"},"actor":{"login":"mallory"}}]});
        for field in ["userContentEdits", "timelineItems"] {
            let mut edited = text();
            edited[field] = bad.clone();
            let Written {
                issue: safe,
                comments,
                ..
            } = b.pass().checked(&rest, &edited).unwrap();
            let prompt = b.pass().task("sam/box", &safe, &comments);
            assert!(!prompt.contains("trusted title"), "{field}: {prompt}");
            assert!(!prompt.contains("trusted body"), "{field}: {prompt}");
        }
        let mut edited = text();
        edited["comments"]["nodes"][0]["userContentEdits"] = bad;
        let Written {
            issue: safe,
            comments,
            ..
        } = b.pass().checked(&rest, &edited).unwrap();
        assert!(
            !b.pass()
                .task("sam/box", &safe, &comments)
                .contains("trusted reply")
        );
    }
    #[test]
    fn authorized_snapshot_reaches_the_job() {
        let b = Box::new();
        let Written {
            issue: safe,
            comments,
            ..
        } = b.pass().checked(&issue(7, "sam", &[]), &text()).unwrap();
        let prompt = b.pass().task("sam/box", &safe, &comments);
        for s in ["trusted title", "trusted body", "trusted reply"] {
            assert!(prompt.contains(s));
        }
    }
    #[test]
    fn incomplete_or_hidden_history_is_not_authorized() {
        let b = Box::new();
        let rest = issue(7, "sam", &[]);
        for history in [
            Value::Null,
            json!({"totalCount":101,"nodes":[]}),
            json!({"totalCount":1,"nodes":[{"editor":null}]}),
            json!({"totalCount":1,"nodes":[{"editor":{"login":"sam"},"deletedBy":{"login":"mallory"}}]}),
        ] {
            let mut t = text();
            t["userContentEdits"] = history;
            assert_eq!(
                b.pass().checked(&rest, &t).unwrap().issue["title"],
                "Issue text withheld"
            );
        }
        let mut t = text();
        t["comments"]["totalCount"] = json!(2);
        assert!(b.pass().checked(&rest, &t).is_none());
    }

    #[test]
    fn graphql_bot_names_match_rest_authors() {
        let mut b = Box::new();
        b.cfg
            .inbox
            .as_mut()
            .unwrap()
            .authors
            .push("worker[bot]".into());
        let bot = json!({"login":"worker","__typename":"Bot"});
        let mut t = text();
        t["author"] = bot.clone();
        t["userContentEdits"] = json!({"totalCount":1,"nodes":[{"editor":bot}]});
        t["comments"]["nodes"][0]["author"] = bot;
        let Written {
            issue: safe,
            comments,
            ..
        } = b.pass().checked(&issue(7, "worker[bot]", &[]), &t).unwrap();
        assert_eq!(safe["title"], "trusted title");
        assert!(b.pass().heard(&comments[0]));
        // An old bot issue at the cap is still unprovable. Its publisher
        // must replace it, not receive an exception to the trust boundary.
        t["userContentEdits"] = json!({"totalCount":100,"nodes":vec![json!({"editor":{"login":"worker","__typename":"Bot"}});100]});
        assert_eq!(
            b.pass()
                .checked(&issue(7, "worker[bot]", &[]), &t)
                .unwrap()
                .issue["title"],
            "Issue text withheld"
        );
        // Fresh, unedited bot snapshots stay readable however many updates
        // were published as separate comments instead of body edits.
        t["userContentEdits"] = json!({"totalCount":0,"nodes":[]});
        let comment = t["comments"]["nodes"][0].clone();
        t["comments"] = json!({"totalCount":105,"nodes":vec![comment;105]});
        let written = b.pass().checked(&issue(7, "worker[bot]", &[]), &t).unwrap();
        assert_eq!(written.issue["title"], "trusted title");
        assert_eq!(written.comments.len(), 105);
        assert!(written.comments.iter().all(|c| b.pass().heard(c)));
    }
    #[test]
    fn retained_authorized_edits_cannot_hide_a_pruned_outsider() {
        let b = Box::new();
        for count in [99, 100] {
            let history =
                json!({"totalCount":count,"nodes":vec![json!({"editor":{"login":"sam"}}); count]});
            for comment in [false, true] {
                let mut t = text();
                if comment {
                    t["comments"]["nodes"][0]["userContentEdits"] = history.clone();
                } else {
                    t["userContentEdits"] = history.clone();
                }
                let Written {
                    issue, comments, ..
                } = b.pass().checked(&issue(7, "sam", &[]), &t).unwrap();
                let prompt = b.pass().task("sam/box", &issue, &comments);
                let retained = if comment {
                    "trusted reply"
                } else {
                    "trusted body"
                };
                assert_eq!(
                    prompt.contains(retained),
                    count < 100,
                    "{count} edits, comment={comment}"
                );
            }
        }
    }
}
