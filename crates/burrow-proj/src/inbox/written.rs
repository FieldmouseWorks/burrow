//! Text and its edit history are read together before crossing the job boundary.
use super::Pass;
use serde_json::{Value, json};

const QUERY: &str = "query($owner:String!,$name:String!,$n:Int!,$cursor:String){repository(owner:$owner,name:$name){issue(number:$n){author{login __typename ... on User { databaseId } ... on Bot { databaseId } ... on Organization { databaseId }} title body userContentEdits(first:100){totalCount nodes{editor{login __typename ... on User { databaseId } ... on Bot { databaseId } ... on Organization { databaseId }} deletedBy{login __typename ... on User { databaseId } ... on Bot { databaseId } ... on Organization { databaseId }}}} timelineItems(itemTypes:[RENAMED_TITLE_EVENT],first:100){totalCount nodes{... on RenamedTitleEvent{actor{login __typename ... on User { databaseId } ... on Bot { databaseId } ... on Organization { databaseId }}}}} comments(first:100,after:$cursor){pageInfo{hasNextPage endCursor} totalCount nodes{databaseId author{login __typename ... on User { databaseId } ... on Bot { databaseId } ... on Organization { databaseId }} body createdAt userContentEdits(first:100){totalCount nodes{editor{login __typename ... on User { databaseId } ... on Bot { databaseId } ... on Organization { databaseId }} deletedBy{login __typename ... on User { databaseId } ... on Bot { databaseId } ... on Organization { databaseId }}}}}}}}}";
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
        self.allowed(actor["databaseId"].as_u64())
    }
    /// A writer of the issue's own text: an author, or the machinery App
    /// that may open issues. Comments keep `writer`.
    fn issue_writer(&self, actor: &Value) -> bool {
        let id = actor["databaseId"].as_u64();
        self.allowed(id)
            || self
                .cfg
                .machinery_account
                .as_ref()
                .is_some_and(|a| a.matches(id))
    }
    fn writers(&self, list: &Value, field: &str, issue: bool) -> bool {
        let ok = |a: &Value| {
            if issue {
                self.issue_writer(a)
            } else {
                self.writer(a)
            }
        };
        list["nodes"].as_array().is_some_and(|nodes| {
            // GitHub prunes intermediate body edits once 100 are retained.
            // Equality at that limit no longer proves that every editor is known.
            nodes.len() < 100
                && list["totalCount"].as_u64() == Some(nodes.len() as u64)
                && nodes
                    .iter()
                    .all(|e| ok(&e[field]) && (e["deletedBy"].is_null() || ok(&e["deletedBy"])))
        })
    }
    fn authored(&self, text: &Value) -> bool {
        self.writer(&text["author"]) && self.writers(&text["userContentEdits"], "editor", false)
    }
    pub(super) fn written(&self, repo: &str, rest: &Value) -> Option<Written> {
        let (owner, name) = repo.split_once('/')?;
        let mut cursor = None;
        let mut comments = Vec::new();
        loop {
            let got = crate::github::query_for(
                self.cfg,
                repo,
                QUERY,
                json!({"owner":owner,"name":name,"n":rest["number"],"cursor":cursor}),
            )?;
            let mut issue = got["repository"]["issue"].clone();
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
            return self.checked(repo, rest, &issue);
        }
    }

    fn checked(&self, repo: &str, rest: &Value, issue: &Value) -> Option<Written> {
        let nodes = issue["comments"]["nodes"].as_array()?;
        // Fail closed if pagination did not establish the complete history.
        if issue["comments"]["totalCount"].as_u64() != Some(nodes.len() as u64) {
            return None;
        }
        let mut safe = rest.clone();
        // The machinery's extra authority covers only issues it opened itself.
        let by_machine = !self.allowed(issue["author"]["databaseId"].as_u64())
            && self.issue_writer(&issue["author"]);
        if (self.issue_writer(&issue["author"]) || self.generated_child(repo, rest))
            && self.writers(&issue["userContentEdits"], "editor", by_machine)
            && self.writers(&issue["timelineItems"], "actor", by_machine)
        {
            safe["title"] = issue["title"].clone();
            safe["body"] = issue["body"].clone();
        } else {
            safe["title"] = json!("Issue text withheld");
            safe["body"] = json!(
                "The issue's title or body has an untrusted or unknown writer. Ask an authorized author for a new issue."
            );
        }
        let comments = nodes.iter().filter(|c| self.authored(c)).map(|c| json!({"id":c["databaseId"], "user":{"id":c["author"]["databaseId"],"type":c["author"]["__typename"],"login": if c["author"]["__typename"] == "Bot" { json!(format!("{}[bot]", c["author"]["login"].as_str().unwrap_or_default().trim_end_matches("[bot]"))) } else { c["author"]["login"].clone() }}, "body":c["body"], "created_at":c["createdAt"]})).collect();
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
    #[test]
    fn text_verification_routes_every_page_to_its_repository() {
        use std::fs;
        for repo in ["FieldmouseWorks/box", "TusanHomichi/box"] {
            let b = Box::new();
            // Jobs, continuations and triage all use written(). Require the
            // route on both its first read and its comment pagination.
            let gh = &b.cfg.tools.gh;
            let script = fs::read_to_string(gh).unwrap();
            let guard = format!(
                "[ \"$GH_REPO\" = '{repo}' ] || exit 1\nprintf '%s\\n' \"$GH_REPO\" >>'{}'\n",
                b.path("routes").display()
            );
            fs::write(
                gh,
                script.replacen("#!/bin/sh\n", &format!("#!/bin/sh\n{guard}"), 1),
            )
            .unwrap();
            let graphql = b.path("graphql");
            let script = fs::read_to_string(&graphql).unwrap();
            // First page asks for another page; second page completes the
            // same empty comment list. Keep the real fixture's provenance.
            fs::write(&graphql, format!("{} | jq --arg args \"$*\" '.data.repository.issue.comments.pageInfo = (if ($args | contains(\"cursor=\")) then {{hasNextPage:false,endCursor:null}} else {{hasNextPage:true,endCursor:\"next\"}} end)'\n", script.trim_end())).unwrap();
            assert!(b.pass().written(repo, &issue(7, "sam", &[])).is_some());
            assert_eq!(
                fs::read_to_string(b.path("routes")).unwrap(),
                format!("{repo}\n{repo}\n")
            );
        }
    }
    fn text() -> Value {
        let edits = json!({"totalCount":0,"nodes":[]});
        json!({"author":burrow_github::fake::actor("sam"),"title":"trusted title","body":"trusted body", "userContentEdits":edits,"timelineItems":edits,"comments":{"totalCount":1,"nodes":[{"databaseId":9,"author":burrow_github::fake::actor("sam"),"body":"trusted reply","createdAt":"2026-11-01T00:00:00Z","userContentEdits":edits}]}})
    }
    #[test]
    fn a_reused_login_does_not_authorize_any_issue_text() {
        let b = Box::new();
        let mut rest = issue(7, "sam", &["claude"]);
        rest["user"]["id"] = json!(999);
        let mut exposed = vec![("opener", b.pass().eligible("sam/box", &rest))];
        for role in ["author", "editor", "actor", "commenter", "deleter"] {
            let mut t = text();
            let spoof = json!({"login":"sam","databaseId":999});
            match role {
                "author" => t["author"] = spoof,
                "editor" => {
                    t["userContentEdits"] = json!({"totalCount":1,"nodes":[{"editor":spoof}]})
                }
                "deleter" => {
                    t["userContentEdits"] = json!({"totalCount":1,"nodes":[{"editor":burrow_github::fake::actor("sam"),"deletedBy":spoof}]})
                }
                "actor" => t["timelineItems"] = json!({"totalCount":1,"nodes":[{"actor":spoof}]}),
                _ => t["comments"]["nodes"][0]["author"] = spoof,
            }
            let written = b.pass().checked("sam/box", &rest, &t).unwrap();
            if role == "commenter" {
                exposed.push((role, !written.comments.is_empty()));
            } else {
                exposed.push((role, written.issue["title"] != "Issue text withheld"));
            }
        }
        assert!(
            exposed.iter().all(|(_, unsafe_text)| !unsafe_text),
            "{exposed:?}"
        );
    }
    #[test]
    fn machinery_issue_text_is_verified_but_its_comments_are_not_heard() {
        let mut b = Box::new();
        b.cfg.machinery_account = Some(burrow_github::fake::author("workers[bot]"));
        let id = burrow_github::fake::author("workers[bot]").id;
        let app = json!({"login":"workers","__typename":"Bot","databaseId":id});
        let rest = issue(7, "workers[bot]", &[]);
        let mut t = text();
        t["author"] = app.clone();
        let read = |t: &Value| b.pass().checked("sam/box", &rest, t).unwrap();
        assert_eq!(read(&t).issue["title"], "trusted title");
        // Its edits, title changes and deletions of edits are its own.
        let mut e = t.clone();
        e["userContentEdits"] = json!({"totalCount":1,"nodes":[{"editor":app,"deletedBy":app}]});
        e["timelineItems"] = json!({"totalCount":1,"nodes":[{"actor":app}]});
        assert_eq!(read(&e).issue["title"], "trusted title");
        // A stranger with the same login, or in the history, withholds it.
        let spoof = json!({"login":"workers","__typename":"Bot","databaseId":999});
        let mut s = t.clone();
        s["author"] = spoof.clone();
        assert_eq!(read(&s).issue["title"], "Issue text withheld");
        for (field, key) in [("userContentEdits", "editor"), ("timelineItems", "actor")] {
            let mut s = t.clone();
            s[field] = json!({"totalCount":1,"nodes":[{key:spoof}]});
            assert_eq!(read(&s).issue["title"], "Issue text withheld", "{field}");
        }
        let mut s = t.clone();
        s["userContentEdits"] = json!({"totalCount":1,"nodes":[{"editor":app,"deletedBy":spoof}]});
        assert_eq!(read(&s).issue["title"], "Issue text withheld");
        // Incomplete history still fails closed.
        let mut s = t.clone();
        s["userContentEdits"] = json!({"totalCount":101,"nodes":[]});
        assert_eq!(read(&s).issue["title"], "Issue text withheld");
        // Comments stay author-only, even from the App.
        let mut c = t.clone();
        c["comments"]["nodes"][0]["author"] = app.clone();
        assert!(read(&c).comments.is_empty());
        // On a human's issue, the App's edit is not trusted.
        let mut h = text();
        h["userContentEdits"] =
            json!({"totalCount":1,"nodes":[{"editor":app.clone(),"deletedBy":null}]});
        let human = issue(7, "sam", &[]);
        assert_eq!(
            b.pass().checked("sam/box", &human, &h).unwrap().issue["title"],
            "Issue text withheld"
        );
        // An outside-labelled machinery issue is still not eligible.
        let outside = issue(7, "workers[bot]", &["outside text"]);
        assert!(!b.pass().eligible("sam/box", &outside));
    }
    #[test]
    fn an_account_keeps_its_authority_after_a_login_change() {
        let b = Box::new();
        let mut rest = issue(7, "sam", &[]);
        rest["user"]["login"] = json!("renamed-account");
        assert!(b.pass().eligible("sam/box", &rest));
        let mut t = text();
        t["author"]["login"] = json!("renamed-account");
        t["comments"]["nodes"][0]["author"]["login"] = json!("renamed-account");
        let written = b.pass().checked("sam/box", &rest, &t).unwrap();
        assert_eq!(written.issue["title"], "trusted title");
        assert_eq!(written.comments.len(), 1);
        assert!(b.pass().heard(&written.comments[0]));
    }
    #[test]
    fn outside_edits_never_reach_a_job() {
        let b = Box::new();
        let rest = issue(7, "sam", &["claude"]);
        let bad = json!({"totalCount":1,"nodes":[{"editor":burrow_github::fake::actor("mallory"),"actor":burrow_github::fake::actor("mallory")}]});
        for field in ["userContentEdits", "timelineItems"] {
            let mut edited = text();
            edited[field] = bad.clone();
            let Written {
                issue: safe,
                comments,
                ..
            } = b.pass().checked("sam/box", &rest, &edited).unwrap();
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
        } = b.pass().checked("sam/box", &rest, &edited).unwrap();
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
        } = b
            .pass()
            .checked("sam/box", &issue(7, "sam", &[]), &text())
            .unwrap();
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
            json!({"totalCount":1,"nodes":[{"editor":burrow_github::fake::actor("sam"),"deletedBy":burrow_github::fake::actor("mallory")}]}),
        ] {
            let mut t = text();
            t["userContentEdits"] = history;
            assert_eq!(
                b.pass().checked("sam/box", &rest, &t).unwrap().issue["title"],
                "Issue text withheld"
            );
        }
        let mut t = text();
        t["comments"]["totalCount"] = json!(2);
        assert!(b.pass().checked("sam/box", &rest, &t).is_none());
    }

    #[test]
    fn graphql_bot_ids_match_rest_authors() {
        let mut b = Box::new();
        b.cfg
            .inbox
            .as_mut()
            .unwrap()
            .authors
            .push(burrow_github::fake::author("worker[bot]"));
        let bot = json!({"login":"worker","__typename":"Bot","databaseId":burrow_github::fake::author("worker[bot]").id});
        assert!(!b.pass().writer(&json!({"login":"worker", "__typename":"User", "databaseId":burrow_github::fake::author("worker").id})));
        let mut t = text();
        t["author"] = bot.clone();
        t["userContentEdits"] = json!({"totalCount":1,"nodes":[{"editor":bot}]});
        t["comments"]["nodes"][0]["author"] = bot;
        let Written {
            issue: safe,
            comments,
            ..
        } = b
            .pass()
            .checked("sam/box", &issue(7, "worker[bot]", &[]), &t)
            .unwrap();
        assert_eq!(safe["title"], "trusted title");
        assert!(b.pass().heard(&comments[0]));
        // An old bot issue at the cap is still unprovable. Its publisher
        // must replace it, not receive an exception to the trust boundary.
        t["userContentEdits"] = json!({"totalCount":100,"nodes":vec![json!({"editor":{"login":"worker","__typename":"Bot","databaseId":burrow_github::fake::author("worker[bot]").id}});100]});
        assert_eq!(
            b.pass()
                .checked("sam/box", &issue(7, "worker[bot]", &[]), &t)
                .unwrap()
                .issue["title"],
            "Issue text withheld"
        );
        // Fresh, unedited bot snapshots stay readable however many updates
        // were published as separate comments instead of body edits.
        t["userContentEdits"] = json!({"totalCount":0,"nodes":[]});
        let comment = t["comments"]["nodes"][0].clone();
        t["comments"] = json!({"totalCount":105,"nodes":vec![comment;105]});
        let written = b
            .pass()
            .checked("sam/box", &issue(7, "worker[bot]", &[]), &t)
            .unwrap();
        assert_eq!(written.issue["title"], "trusted title");
        assert_eq!(written.comments.len(), 105);
        assert!(written.comments.iter().all(|c| b.pass().heard(c)));
    }
    #[test]
    fn retained_authorized_edits_cannot_hide_a_pruned_outsider() {
        let b = Box::new();
        for count in [99, 100] {
            let history = json!({"totalCount":count,"nodes":vec![json!({"editor":burrow_github::fake::actor("sam")}); count]});
            for comment in [false, true] {
                let mut t = text();
                if comment {
                    t["comments"]["nodes"][0]["userContentEdits"] = history.clone();
                } else {
                    t["userContentEdits"] = history.clone();
                }
                let Written {
                    issue, comments, ..
                } = b
                    .pass()
                    .checked("sam/box", &issue(7, "sam", &[]), &t)
                    .unwrap();
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
