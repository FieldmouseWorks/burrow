//! A parent's plain task list is the sequencer's only memory.
use super::{Pass, dirs, key, raw};
use crate::{
    github::{api, every},
    planning::Task,
    read, swap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{fs, path::PathBuf};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Child {
    number: Option<u64>,
    attempted: bool,
    merged_pr: Option<String>,
    #[serde(default)]
    offered: bool,
}
#[derive(Deserialize, Serialize)]
struct List {
    tasks: Vec<Task>,
    children: Vec<Child>,
    comment: Option<u64>,
    rendered: String,
}
impl List {
    fn next(&self) -> Option<usize> {
        self.children.iter().position(|c| c.merged_pr.is_none())
    }
    fn checklist(&self, repo: &str, parent: &str) -> String {
        let mut text = format!(
            "Task list for {repo}#{parent}. Each implementation child is one fresh session and one PR.\n"
        );
        for (task, child) in self.tasks.iter().zip(&self.children) {
            let check = if child.merged_pr.is_some() { "x" } else { " " };
            let number = child
                .number
                .map_or("(creating)".into(), |n| format!("#{n}"));
            let pr = child
                .merged_pr
                .as_ref()
                .map_or(String::new(), |pr| format!(" — {pr}"));
            text.push_str(&format!("\n- [{check}] {number}: {}{pr}", task.title));
        }
        text
    }
}

impl Pass<'_> {
    fn child_body(repo: &str, parent: &str, index: usize, task: &Task) -> String {
        format!(
            "Child of https://github.com/{repo}/issues/{parent}. Finish only this task in one fresh session and one PR. The parent's original authorization boundaries still apply; this child grants no additional authority. Do not read earlier sessions' transcripts.\n\n{}\n\nVerification: {}\n\nIf this is too big, publish any progress, split it with `proj split FILE` (a JSON array of title/body/check tasks), then end the session. Wait for the PR to land before declaring this task done.\n\n<!-- proj-child {repo}#{parent} {} -->",
            task.body,
            task.check,
            index + 1
        )
    }
    /// Size a job once before it starts, not again for every PR reply.
    pub(super) fn started_issue_job(&self, repo: Option<&str>, n: &str) -> bool {
        repo.is_some_and(|repo| {
            self.inbox
                .state
                .join(format!("{}-{n}", key(repo)))
                .join("job")
                .exists()
        })
    }
    pub(super) fn worker_split_pending(&self, repo: Option<&str>, n: &str) -> bool {
        repo.is_some_and(|repo| self.plan_dir(repo, n).join("worker-split").exists())
    }
    pub(super) fn begin_worker_split(&self, repo: &str, issue: &Value) -> bool {
        let n = raw(&issue["number"]);
        let Some(input) = self.input(repo, issue) else {
            return false;
        };
        let record = self.inbox.state.join(format!("{}-{n}", key(repo)));
        if !Self::handed(&record, &input) || self.keep_planned(repo, &n, &input).is_err() {
            return false;
        }
        fs::write(
            self.plan_dir(repo, &raw(&issue["number"]))
                .join("worker-split"),
            "pending\n",
        )
        .is_ok()
    }
    pub(super) fn has_task_list(&self, repo: Option<&str>, n: &str) -> bool {
        repo.is_some_and(|repo| self.plan_dir(repo, n).join("tasks.json").exists())
    }
    fn render_list(&self, repo: &str, n: &str, list: &mut List) -> bool {
        let text = list.checklist(repo, n);
        if text == list.rendered {
            return true;
        }
        let marker = format!("<!-- proj-plan {repo}#{n} -->");
        let body = self.signed(&format!("{text}\n\n{marker}"));
        if list.comment.is_none() {
            let Some(comments) = every(
                self.cfg,
                &format!("repos/{repo}/issues/{n}/comments?per_page=100"),
            ) else {
                return false;
            };
            list.comment = comments
                .iter()
                .find(|c| {
                    self.allowed(c["user"]["login"].as_str().unwrap_or_default())
                        && c["body"].as_str().is_some_and(|b| b.contains(&marker))
                })
                .and_then(|c| c["id"].as_u64());
        }
        let (method, endpoint) = match list.comment {
            Some(id) => ("PATCH", format!("repos/{repo}/issues/comments/{id}")),
            None => ("POST", format!("repos/{repo}/issues/{n}/comments")),
        };
        let Some(response) = crate::github::reported(burrow_github::call_once(
            &mut crate::github::client(self.cfg),
            method,
            &endpoint,
            Some(&json!({"body":body})),
        )) else {
            return false;
        };
        if list.comment.is_none() {
            list.comment = response.json().ok().and_then(|v| v["id"].as_u64());
            if list.comment.is_none() {
                return false;
            }
        }
        list.rendered = text;
        self.save_list(repo, n, list)
    }

    /// A child cannot bypass its parent by receiving an engine/ready label.
    pub(super) fn child_admissible(&self, repo: &str, n: &str) -> bool {
        self.child_admissible_at(repo, n, 0)
    }
    fn child_admissible_at(&self, repo: &str, n: &str, depth: usize) -> bool {
        if depth >= 64 {
            return false;
        }
        if !self.inbox.planning {
            return true;
        }
        for d in dirs(&self.inbox.state.join(".plans"), &format!("{}-", key(repo))) {
            let Ok(bytes) = fs::read(d.join("tasks.json")) else {
                continue;
            };
            let Ok(list) = serde_json::from_slice::<List>(&bytes) else {
                return false;
            };
            let Some(index) = list
                .children
                .iter()
                .position(|c| c.number.map(|v| v.to_string()).as_deref() == Some(n))
            else {
                continue;
            };
            let Some(parent) = d
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| s.strip_prefix(&format!("{}-", key(repo))))
            else {
                return false;
            };
            let Some(issue) = api(self.cfg, &format!("repos/{repo}/issues/{parent}")) else {
                return false;
            };
            if !self.eligible(&issue)
                || issue["state"] != "open"
                || !self.same_source(repo, &issue)
                || list.next() != Some(index)
                || !self.child_admissible_at(repo, parent, depth + 1)
            {
                return false;
            }
        }
        true
    }
    pub(super) fn child_can_close(&self, repo: &str, n: &str) -> bool {
        if !self.inbox.planning {
            return true;
        }
        for d in dirs(&self.inbox.state.join(".plans"), &format!("{}-", key(repo))) {
            let Ok(bytes) = fs::read(d.join("tasks.json")) else {
                continue;
            };
            let Ok(list) = serde_json::from_slice::<List>(&bytes) else {
                return false;
            };
            if list
                .children
                .iter()
                .any(|c| c.number.map(|v| v.to_string()).as_deref() == Some(n))
            {
                return n
                    .parse()
                    .ok()
                    .and_then(|n| self.merged_child(repo, n))
                    .is_some();
            }
        }
        true
    }
    fn plan_dir(&self, repo: &str, n: &str) -> PathBuf {
        self.inbox
            .state
            .join(".plans")
            .join(format!("{}-{n}", key(repo)))
    }
    pub(super) fn planned(&self, repo: Option<&str>, issue: &Value) -> bool {
        if !self.inbox.planning {
            return true;
        }
        let Some(repo) = repo else { return false };
        let n = raw(&issue["number"]);
        if self.plan_dir(repo, &n).join("tasks.json").exists() {
            return false;
        }
        self.same_source(repo, issue)
    }
    fn same_source(&self, repo: &str, issue: &Value) -> bool {
        let n = raw(&issue["number"]);
        let Some(had) = read(&self.plan_dir(repo, &n).join("input")) else {
            return false;
        };
        self.input(repo, issue)
            .is_some_and(|input| input.trim_end() == had)
    }
    pub(super) fn keep_planned(&self, repo: &str, n: &str, input: &str) -> std::io::Result<()> {
        let d = self.plan_dir(repo, n);
        fs::create_dir_all(&d)?;
        swap(&d.join("input"), input)
    }
    fn save_list(&self, repo: &str, n: &str, list: &List) -> bool {
        serde_json::to_string_pretty(list)
            .ok()
            .is_some_and(|text| swap(&self.plan_dir(repo, n).join("tasks.json"), &text).is_ok())
    }
    /// Reconcile a POST whose answer was lost by its immutable receipt.
    /// Never re-POST an ambiguous creation: if it is not found, retain the
    /// task and retry reconciliation, rather than run or lose it.
    pub(super) fn split_children(&self, repo: &str, n: &str, tasks: &[Task]) -> bool {
        let d = self.plan_dir(repo, n);
        if fs::create_dir_all(&d).is_err() {
            return false;
        }
        let kept = d.join("tasks.json");
        let mut list: List = match fs::read(&kept) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(list) => list,
                Err(_) => return false,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => List {
                tasks: tasks.to_vec(),
                children: vec![Child::default(); tasks.len()],
                comment: None,
                rendered: String::new(),
            },
            Err(_) => return false,
        };
        if list.tasks != tasks {
            return false;
        }
        if !self.save_list(repo, n, &list) {
            return false;
        }
        for i in 0..tasks.len() {
            if list.children[i].number.is_some() {
                continue;
            }
            if let Some(existing) = tasks[i].issue {
                if existing.to_string() == n {
                    return false;
                }
                let Some(issue) = api(self.cfg, &format!("repos/{repo}/issues/{existing}")) else {
                    return false;
                };
                if !self.eligible(&issue) || issue.get("pull_request").is_some() {
                    return false;
                }
                list.children[i].number = Some(existing);
                if !self.save_list(repo, n, &list) {
                    return false;
                }
                continue;
            }
            let marker = format!("<!-- proj-child {repo}#{n} {} -->", i + 1);
            let Some(all) = every(
                self.cfg,
                &format!("repos/{repo}/issues?state=all&per_page=100"),
            ) else {
                return false;
            };
            let matches: Vec<_> = all
                .iter()
                .filter(|issue| {
                    self.eligible(issue)
                        && issue.get("pull_request").is_none()
                        && issue["body"]
                            .as_str()
                            .is_some_and(|body| body.ends_with(&marker))
                })
                .collect();
            if matches.len() > 1 {
                return false;
            }
            if let Some(found) = matches.first() {
                list.children[i].number = found["number"].as_u64();
            } else if list.children[i].attempted {
                return false;
            } else {
                list.children[i].attempted = true;
                if !self.save_list(repo, n, &list) {
                    return false;
                }
                let task = &tasks[i];
                let body = Self::child_body(repo, n, i, task);
                let result = burrow_github::call_once(
                    &mut crate::github::client(self.cfg),
                    "POST",
                    &format!("repos/{repo}/issues"),
                    Some(&json!({"title":task.title,"body":body,"labels":["later"]})),
                );
                match result {
                    Ok(response) => {
                        list.children[i].number =
                            response.json().ok().and_then(|v| v["number"].as_u64());
                    }
                    Err(error) => {
                        if !error.unanswered {
                            list.children[i].attempted = false;
                            let _ = self.save_list(repo, n, &list);
                        }
                        eprintln!("proj split: {error}");
                        return false;
                    }
                }
            }
            if list.children[i].number.is_none() || !self.save_list(repo, n, &list) {
                return false;
            }
        }
        for i in 0..tasks.len() {
            // The parent already sized this exact generated child. Retain
            // that approval instead of paying a second planner to repeat it.
            let number = list.children[i].number.unwrap().to_string();
            if self.plan_dir(repo, &number).join("input").exists() {
                continue;
            }
            let Some(child) = api(self.cfg, &format!("repos/{repo}/issues/{number}")) else {
                return false;
            };
            let expected = if tasks[i].issue.is_some() {
                tasks[i].body.clone()
            } else {
                Self::child_body(repo, n, i, &tasks[i])
            };
            if child["title"].as_str() == Some(tasks[i].title.as_str())
                && child["body"].as_str() == Some(expected.as_str())
                && self.eligible(&child)
            {
                let input = self.issue_text(repo, &child, &[]);
                if self.keep_planned(repo, &number, &input).is_err() {
                    return false;
                }
            }
        }
        if !self.render_list(repo, n, &mut list) {
            return false;
        }
        for label in self.inbox.labels.keys() {
            if !self.remove_label(repo, n, label) {
                return false;
            }
        }
        true
    }

    /// Called by the repo's normal event pass. No job/session is the parent.
    pub(super) fn sequence(&mut self, repo: &str, all: &[Value]) {
        if !self.inbox.planning {
            return;
        }
        for d in dirs(&self.inbox.state.join(".plans"), &format!("{}-", key(repo))) {
            let Some(name) = d.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some(n) = name.strip_prefix(&format!("{}-", key(repo))) else {
                continue;
            };
            let Some(parent) = all
                .iter()
                .find(|i| raw(&i["number"]) == n && self.eligible(i))
            else {
                continue;
            };
            let Ok(bytes) = fs::read(d.join("tasks.json")) else {
                continue;
            };
            let Ok(mut list) = serde_json::from_slice::<List>(&bytes) else {
                self.retry(repo);
                continue;
            };
            if !self.same_source(repo, parent) {
                let stale = d.join("stale-notice");
                if !stale.exists() && self.post(repo, n, "The parent task changed after its split. Child admission is paused; the existing task list and completed work are retained for replanning.") {
                    let _ = fs::write(stale, "paused\n");
                }
                continue;
            }
            if !self.render_list(repo, n, &mut list) {
                self.miss(repo);
                continue;
            }
            if list.children.iter().any(|c| c.number.is_none()) {
                continue;
            }
            let Some(index) = list.next() else {
                if !self.close(repo, n, "completed") {
                    self.miss(repo);
                }
                continue;
            };
            let child = list.children[index].number.unwrap();
            let Some(issue) = api(self.cfg, &format!("repos/{repo}/issues/{child}")) else {
                self.miss(repo);
                continue;
            };
            if !self.eligible(&issue) {
                continue;
            }
            let mut advanced = false;
            if issue["state"] == "closed" {
                if let Some(pr) = self.merged_child(repo, child) {
                    list.children[index].merged_pr = Some(pr);
                    if !self.save_list(repo, n, &list) {
                        self.retry(repo);
                        continue;
                    }
                    advanced = true;
                }
            } else if !list.children[index].offered
                && self.engine(&issue).is_none()
                && !self
                    .plan_dir(repo, &child.to_string())
                    .join("tasks.json")
                    .exists()
            {
                if !self.add_label(repo, &child.to_string(), "ready")
                    || !self.remove_label(repo, &child.to_string(), "later")
                {
                    self.miss(repo);
                } else {
                    list.children[index].offered = true;
                    if !self.save_list(repo, n, &list) {
                        self.retry(repo);
                    }
                }
            }
            if advanced {
                if self.render_list(repo, n, &mut list) {
                    // Next pass admits the next child from the persisted list.
                    self.book(repo, crate::now());
                } else {
                    self.miss(repo);
                }
            }
        }
    }
    fn merged_child(&self, repo: &str, n: u64) -> Option<String> {
        if let Ok(bytes) = fs::read(self.plan_dir(repo, &n.to_string()).join("tasks.json"))
            && let Ok(list) = serde_json::from_slice::<List>(&bytes)
            && !list.children.is_empty()
        {
            return list
                .next()
                .is_none()
                .then(|| format!("https://github.com/{repo}/issues/{n}"));
        }
        let (owner, name) = repo.split_once('/')?;
        let query = "query($owner:String!,$name:String!,$n:Int!){repository(owner:$owner,name:$name){issue(number:$n){closedByPullRequestsReferences(first:100,includeClosedPrs:true){nodes{merged url}}}}}";
        let got = crate::github::query_for(
            self.cfg,
            repo,
            query,
            json!({"owner":owner,"name":name,"n":n}),
        )?;
        got["repository"]["issue"]["closedByPullRequestsReferences"]["nodes"]
            .as_array()?
            .iter()
            .find(|pr| pr["merged"] == true)
            .and_then(|pr| pr["url"].as_str())
            .map(str::to_owned)
    }
}

#[cfg(test)]
mod tests {
    use super::super::sorting_tests::{Box, issue};
    use super::*;

    fn parent_fixture() -> (Box, Value, List) {
        let mut b = Box::new();
        b.cfg.inbox.as_mut().unwrap().planning = true;
        let mut parent = issue(7, "sam", &["tracker"]);
        parent["state"] = json!("open");
        fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
        let p = b.pass();
        let source = p.issue_text("sam/box", &parent, &[]);
        p.keep_planned("sam/box", "7", &source).unwrap();
        let mut list = List {
            tasks: (0..2)
                .map(|i| Task {
                    issue: None,
                    title: format!("task {i}"),
                    body: "one PR".into(),
                    check: "regression".into(),
                })
                .collect(),
            children: vec![
                Child {
                    number: Some(8),
                    ..Child::default()
                },
                Child {
                    number: Some(9),
                    ..Child::default()
                },
            ],
            comment: Some(100),
            rendered: String::new(),
        };
        list.rendered = list.checklist("sam/box", "7");
        assert!(p.save_list("sam/box", "7", &list));
        (b, parent, list)
    }

    #[test]
    fn events_drain_one_child_and_only_a_landed_pr_advances_the_parent() {
        let (b, parent, _) = parent_fixture();
        let mut child = issue(8, "sam", &["later"]);
        child["state"] = json!("open");
        fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(
            b.log().contains("issues/8/labels -f labels[]=ready"),
            "{}",
            b.log()
        );
        assert!(!b.log().contains("issues/9/labels"));
        assert!(b.pass().child_admissible("sam/box", "8"));
        assert!(!b.pass().child_admissible("sam/box", "9"));
        let ready = |n| {
            let mut issue = super::super::sorting_tests::issue(n, "sam", &["ready"]);
            issue["repository_url"] = json!("https://api.github.com/repos/sam/box");
            issue["created_at"] = json!("2026-11-01T00:00:00Z");
            issue
        };
        let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
        assert_eq!(b.pass().pick_ready(t, &[ready(9)]), None);
        assert_eq!(
            b.pass().pick_ready(t, &[ready(8)]),
            Some(("sam/box".into(), "8".into()))
        );
        // Closing an issue alone does not release another implementation.
        child["state"] = json!("closed");
        fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(!b.pass().child_admissible("sam/box", "9"));
        assert!(!b.pass().child_can_close("sam/box", "8"));
        fs::write(b.path("merged-8"), json!({"data":{"repository":{"issue":{"closedByPullRequestsReferences":{"nodes":[{"merged":true,"url":"https://github.com/sam/box/pull/10"}]}}}}}).to_string()).unwrap();
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        assert!(b.pass().child_admissible("sam/box", "9"));
        assert_eq!(
            b.pass().pick_ready(t, &[ready(9)]),
            Some(("sam/box".into(), "9".into()))
        );
        assert!(b.pass().child_can_close("sam/box", "8"));
        let stored: List = serde_json::from_slice(
            &fs::read(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap(),
        )
        .unwrap();
        assert!(stored.checklist("sam/box", "7").contains("[x] #8"));
        assert!(stored.checklist("sam/box", "7").contains("/pull/10"));
    }

    #[test]
    fn edited_and_untrusted_parents_cannot_admit_children() {
        for change in ["edit", "outside", "opener"] {
            let (b, mut parent, _) = parent_fixture();
            match change {
                "edit" => parent["body"] = json!("Changed scope"),
                "outside" => parent["labels"] = json!([{"name":"outside text"}]),
                _ => parent["user"]["login"] = json!("stranger"),
            }
            fs::write(b.path("repos_sam_box_issues_7"), parent.to_string()).unwrap();
            assert!(!b.pass().child_admissible("sam/box", "8"), "{change}");
        }
    }

    #[test]
    fn a_partial_split_never_marks_its_parent_ready_for_implementation() {
        let (b, parent, _) = parent_fixture();
        assert!(!b.pass().planned(Some("sam/box"), &parent));
    }
    #[test]
    fn a_deferred_child_is_not_repeatedly_released_and_replanned() {
        let (b, parent, _) = parent_fixture();
        let mut child = issue(8, "sam", &["later"]);
        child["state"] = json!("open");
        fs::write(b.path("repos_sam_box_issues_8"), child.to_string()).unwrap();
        b.pass().sequence("sam/box", std::slice::from_ref(&parent));
        b.pass().sequence("sam/box", &[parent]);
        assert_eq!(
            b.log().matches("issues/8/labels -f labels[]=ready").count(),
            1
        );
    }
    #[test]
    fn an_exact_child_keeps_its_parent_size_approval_without_another_planner() {
        let (b, _parent, list) = parent_fixture();
        for (i, task) in list.tasks.iter().enumerate() {
            let mut child = issue(8 + i as u64, "sam", &["ready"]);
            child["title"] = json!(task.title);
            child["body"] = json!(Pass::child_body("sam/box", "7", i, task));
            fs::write(
                b.path(&format!("repos_sam_box_issues_{}", 8 + i)),
                child.to_string(),
            )
            .unwrap();
        }
        assert!(b.pass().split_children("sam/box", "7", &list.tasks));
        let child: Value =
            serde_json::from_slice(&fs::read(b.path("repos_sam_box_issues_8")).unwrap()).unwrap();
        assert!(b.pass().planned(Some("sam/box"), &child));
    }

    #[test]
    fn lost_creation_answers_are_reconciled_without_another_child_post() {
        let (b, _parent, mut list) = parent_fixture();
        for child in &mut list.children {
            child.number = None;
            child.attempted = true;
        }
        list.rendered.clear();
        assert!(b.pass().save_list("sam/box", "7", &list));
        let found: Vec<_> = (0..2)
            .map(|i| {
                let mut child = issue(8 + i, "sam", &["later"]);
                child["body"] = json!(format!(
                    "task body\n<!-- proj-child sam/box#7 {} -->",
                    i + 1
                ));
                child
            })
            .collect();
        fs::write(b.path("repos_sam_box_issues"), json!(found).to_string()).unwrap();
        assert!(b.pass().split_children("sam/box", "7", &list.tasks));
        assert!(b.pass().split_children("sam/box", "7", &list.tasks));
        assert!(
            !b.log().contains("-X POST repos/sam/box/issues "),
            "{}",
            b.log()
        );
        assert_eq!(
            b.log()
                .matches("-X PATCH repos/sam/box/issues/comments/100")
                .count(),
            1
        );
    }
    #[test]
    fn a_worker_split_posts_its_answer_before_retiring_and_can_retry_a_partial_plan() {
        let (b, parent, list) = parent_fixture();
        b.job_ending("j1", "done", Some("split"), "Split into bounded tasks.");
        let record = b.record("j1");
        fs::write(
            b.cfg.jobs.join("j1/1.split"),
            serde_json::to_string(&list.tasks).unwrap(),
        )
        .unwrap();
        let mut enrolled = parent;
        enrolled["labels"] = json!([{"name":"claude"}]);
        enrolled["repository_url"] = json!("https://api.github.com/repos/sam/box");
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&enrolled))
            .unwrap();
        assert_eq!(read(&record.join("answered")).as_deref(), Some("1"));
        assert!(b.posted().contains("Split into bounded tasks."));
        assert!(b.pass().worker_split_pending(Some("sam/box"), "7"));
        assert!(b.log().contains("labels[]=tracker"));
        let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
        assert!(
            b.pass().sorted(t, &enrolled, &Default::default()),
            "the host resumes the split instead of starting another planner"
        );
        assert!(!b.log().contains("-X POST repos/sam/box/issues "));
    }
    #[test]
    fn a_human_claude_issue_is_sized_on_a_fresh_codex_job_before_implementation() {
        let mut b = Box::new();
        b.cfg.inbox.as_mut().unwrap().planning = true;
        b.cfg.projects = b.path("projects");
        fs::create_dir_all(b.cfg.projects.join("box")).unwrap();
        b.cfg.tools.systemd_run = "true".into();
        b.cfg.tools.systemctl = "false".into();
        b.cfg
            .inbox
            .as_mut()
            .unwrap()
            .triage
            .as_mut()
            .unwrap()
            .sort_unlabelled = false;
        let mut issue = issue(7, "sam", &["claude"]);
        issue["repository_url"] = json!("https://api.github.com/repos/sam/box");
        b.pass()
            .triage(
                "sam/box",
                &[issue, super::super::sorting_tests::issue(8, "sam", &[])],
            )
            .unwrap();
        let record = b
            .cfg
            .inbox
            .as_ref()
            .unwrap()
            .state
            .join(".triage/sam_box-7");
        let id = read(&record.join("job")).expect("a size-planning job");
        let job = crate::job::Job::new(&b.cfg, &id);
        assert_eq!(
            crate::engine::choice(&b.cfg, &job).unwrap().engine,
            crate::engine::Engine::Codex
        );
        assert!(
            !b.cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join(".triage/sam_box-8")
                .exists()
        );
        assert!(!job.path("session").exists());
        assert!(
            read(&job.path("prompt.1"))
                .unwrap()
                .contains("Plan the size")
        );
        assert!(
            !b.cfg
                .inbox
                .as_ref()
                .unwrap()
                .state
                .join("sam_box-7/job")
                .exists()
        );
    }
    #[test]
    fn existing_cutoff_recovery_and_answer_accounting_are_preserved() {
        let (mut b, mut issue, _) = parent_fixture();
        b.cfg.tools.systemctl = "false".into();
        b.cfg.tools.systemd_run = "true".into();
        fs::remove_file(b.pass().plan_dir("sam/box", "7").join("tasks.json")).unwrap();
        b.job_ending("j1", "done", None, "");
        let record = b.record("j1");
        let job = crate::job::Job::new(&b.cfg, "j1");
        fs::remove_file(job.turn("1", "outcome")).unwrap();
        fs::write(
            job.turn("1", "jsonl"),
            "{\"type\":\"system\",\"session_id\":\"s\"}\n",
        )
        .unwrap();
        fs::write(job.path("session"), "s").unwrap();
        assert!(job.cut_off("1"));
        issue["labels"] = json!([{"name":"claude"}]);
        fs::write(b.path("repos_sam_box_issues_7"), issue.to_string()).unwrap();
        b.pass()
            .jobs("sam/box", "box", std::slice::from_ref(&issue))
            .unwrap();
        assert_eq!(job.turns().as_deref(), Some("2"));
        assert!(read(&job.path("prompt.2")).unwrap().contains("cut off"));
        assert_eq!(read(&record.join("answered")).as_deref(), Some("1"));
    }
    #[test]
    fn existing_workers_are_not_resized_on_every_reply() {
        let mut b = Box::new();
        b.cfg.inbox.as_mut().unwrap().planning = true;
        b.job("j1", "done", "unfinished PR #4");
        b.record("j1");
        let mut issue = issue(7, "sam", &["claude"]);
        issue["repository_url"] = json!("https://api.github.com/repos/sam/box");
        assert!(b.pass().started_issue_job(Some("sam/box"), "7"));
        let t = b.cfg.inbox.as_ref().unwrap().triage.as_ref().unwrap();
        assert!(b.pass().sorted(t, &issue, &Default::default()));
    }
    #[test]
    fn a_parent_advances_only_past_a_recorded_merged_pr() {
        let mut list = List {
            tasks: vec![],
            children: vec![Child::default(); 100],
            comment: None,
            rendered: String::new(),
        };
        for n in 0..100 {
            assert_eq!(list.next(), Some(n));
            list.children[n].number = Some(n as u64 + 1);
            assert_eq!(
                list.next(),
                Some(n),
                "an issue number alone is not completion"
            );
            list.children[n].merged_pr = Some(format!("https://github.com/o/r/pull/{n}"));
            let disk = serde_json::to_string(&list).unwrap();
            list = serde_json::from_str(&disk).unwrap();
        }
        assert_eq!(list.next(), None);
    }
}
