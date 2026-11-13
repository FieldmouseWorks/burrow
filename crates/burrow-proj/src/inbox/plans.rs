//! A parent's plain task list is the sequencer's only memory.
use super::{Pass, dirs, key, raw};
use crate::{
    github::{api, every},
    job::Job,
    planning::Task,
    swap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::PathBuf};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Child {
    number: Option<u64>,
    attempted: bool,
    #[serde(default)]
    posting_id: Option<u64>,
    merged_pr: Option<String>,
    #[serde(default)]
    offered: bool,
    #[serde(default)]
    marker: Option<String>,
    /// Closed with no merged PR tied to it: the list waits, and has said so.
    #[serde(default)]
    waits: bool,
}
#[derive(Clone, Deserialize, Serialize)]
struct Retained {
    task: Task,
    child: Child,
}
#[derive(Deserialize, Serialize)]
struct List {
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    retained: Vec<Retained>,
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
        if let Some(child) = self.next().map(|i| &self.children[i])
            && child.waits
            && let Some(n) = child.number
        {
            text.push_str(&format!(
                "\n\n#{n} closed without a merged PR; the list waits. If its work landed some other way, an author records the PR with `proj plan done {repo}#{n} PR-URL`."
            ));
        }
        text
    }
}

impl Pass<'_> {
    fn plan_dirs(&self, repo: &str) -> Vec<PathBuf> {
        dirs(&self.inbox.state.join(".plans"), "")
            .into_iter()
            .filter(|d| {
                d.file_name()
                    .and_then(|s| s.to_str())
                    .and_then(|s| s.rsplit_once('-'))
                    .is_some_and(|(of, n)| {
                        of.eq_ignore_ascii_case(&key(repo)) && n.parse::<u64>().is_ok()
                    })
            })
            .collect()
    }

    /// The split parents in the repo whose children include any of these
    /// issues: a child closing moves its parent's list on ([`Pass::sequence`]).
    pub(super) fn parents_of(&self, repo: &str, children: &BTreeSet<u64>) -> BTreeSet<u64> {
        if !self.inbox.planning {
            return BTreeSet::new();
        }
        self.plan_dirs(repo)
            .iter()
            .filter_map(|d| {
                let n = d.file_name()?.to_str()?.rsplit_once('-')?.1.parse().ok()?;
                let list: List =
                    serde_json::from_slice(&fs::read(d.join("tasks.json")).ok()?).ok()?;
                list.children
                    .iter()
                    .any(|c| c.number.is_some_and(|c| children.contains(&c)))
                    .then_some(n)
            })
            .collect()
    }

    /// Only a locally recorded creation by the saved posting account can
    /// bypass the opener allowlist.
    /// Reused issues and comments receive no additional trust. Edit history
    /// remains checked against configured authors by written().
    pub(super) fn generated_child(&self, repo: &str, issue: &Value) -> bool {
        if !self.inbox.planning {
            return false;
        }
        let Some(number) = issue["number"].as_u64() else {
            return false;
        };
        self.plan_dirs(repo).iter().any(|d| {
            fs::read(d.join("tasks.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<List>(&bytes).ok())
                .is_some_and(|list| {
                    list.tasks
                        .iter()
                        .zip(&list.children)
                        .chain(list.retained.iter().map(|r| (&r.task, &r.child)))
                        .any(|(task, child)| {
                            task.issue.is_none()
                                && child.attempted
                                && child.number == Some(number)
                                && child
                                    .posting_id
                                    .is_some_and(|id| issue["user"]["id"].as_u64() == Some(id))
                        })
                })
        })
    }

    pub(super) fn approved_input(&self, repo: &str, n: &str, input: &str) -> bool {
        !self.has_task_list(Some(repo), n)
            && fs::read_to_string(self.plan_dir(repo, n).join("input"))
                .is_ok_and(|had| had == input && self.list_bound(repo, n, input).unwrap_or(false))
    }
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
        self.started_issue_job(repo, n)
            && repo.is_some_and(|repo| self.plan_dir(repo, n).join("worker-split").exists())
    }
    pub(super) fn begin_worker_split(&self, repo: &str, issue: &Value) -> bool {
        let n = raw(&issue["number"]);
        let Some(input) = self.input(repo, issue) else {
            return false;
        };
        let record = self.inbox.state.join(format!("{}-{n}", key(repo)));
        if !Self::handed(&record, &input) {
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
        repo.is_some_and(|repo| {
            let path = self.plan_dir(repo, n).join("tasks.json");
            if !path.exists() {
                return false;
            }
            fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<List>(&bytes).ok())
                .is_none_or(|list| !list.tasks.is_empty())
        })
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
                .find(|c| c["body"].as_str() == Some(body.as_str()))
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
    pub(super) fn child_admissible(&mut self, repo: &str, n: &str) -> bool {
        self.child_admissible_at(repo, n, 0)
    }
    fn child_admissible_at(&mut self, repo: &str, n: &str, depth: usize) -> bool {
        if depth >= 64 {
            return false;
        }
        if !self.inbox.planning {
            return true;
        }
        for d in self.plan_dirs(repo) {
            let Ok(bytes) = fs::read(d.join("tasks.json")) else {
                continue;
            };
            let Ok(list) = serde_json::from_slice::<List>(&bytes) else {
                return false;
            };
            for old in &list.retained {
                if list
                    .children
                    .iter()
                    .any(|c| c.number.map(|v| v.to_string()).as_deref() == Some(n))
                {
                    break;
                }
                if old.child.number.map(|v| v.to_string()).as_deref() == Some(n) {
                    return false;
                }
                if old.child.attempted && old.child.number.is_none() && !self.observe {
                    let Some(issues) = every(
                        self.cfg,
                        &format!("repos/{repo}/issues?state=all&per_page=100"),
                    ) else {
                        self.miss(repo);
                        return false;
                    };
                    if issues.iter().any(|issue| {
                        raw(&issue["number"]) == n
                            && old.child.marker.as_ref().is_some_and(|marker| {
                                issue["body"]
                                    .as_str()
                                    .is_some_and(|body| body.ends_with(marker))
                            })
                    }) {
                        return false;
                    }
                }
            }
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
                .and_then(|s| s.rsplit_once('-').map(|(_, n)| n))
            else {
                return false;
            };
            if self.claimed(repo).contains(parent) {
                return false;
            }
            // An observing pass asks GitHub nothing: the child's place rests
            // on its plan's order alone. The drain asks about the one it
            // picks, its parents included, before it hands it on.
            if self.observe {
                if list.next() != Some(index) || !self.child_admissible_at(repo, parent, depth + 1)
                {
                    return false;
                }
                continue;
            }
            let Some(issue) = api(self.cfg, &format!("repos/{repo}/issues/{parent}")) else {
                self.miss(repo);
                return false;
            };
            if !self.eligible(repo, &issue)
                || issue["state"] != "open"
                || !self.verified_source(repo, &issue).unwrap_or(false)
                || list.next() != Some(index)
                || !self.child_admissible_at(repo, parent, depth + 1)
            {
                return false;
            }
        }
        true
    }
    /// A child's issue closes with its job only once a merged PR is tied to
    /// it: one that closes it, or one its job awaited ([`Pass::record_awaited`]).
    pub(super) fn child_can_close(&mut self, repo: &str, n: &str, job: Option<&Job>) -> bool {
        if !self.inbox.planning {
            return true;
        }
        for d in self.plan_dirs(repo) {
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
                let Ok(n) = n.parse() else { return false };
                let landed = match self.landed_child(repo, n) {
                    Some(None) => job.map_or(Some(None), |job| self.record_awaited(repo, n, job)),
                    landed => landed,
                };
                return match landed {
                    Some(pr) => pr.is_some(),
                    None => {
                        self.miss(repo);
                        false
                    }
                };
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
    pub(super) fn planned(&mut self, repo: Option<&str>, issue: &Value) -> bool {
        if !self.inbox.planning {
            return true;
        }
        let Some(repo) = repo else { return false };
        let n = raw(&issue["number"]);
        if self.has_task_list(Some(repo), &n) {
            return false;
        }
        // Unavailable verification must never approve implementation.
        self.verified_source(repo, issue).unwrap_or(false)
    }
    pub(super) fn verified_source(&mut self, repo: &str, issue: &Value) -> Option<bool> {
        let result = self.same_source(repo, issue);
        if result.is_none() {
            self.miss(repo);
        }
        result
    }
    fn same_source(&self, repo: &str, issue: &Value) -> Option<bool> {
        let n = raw(&issue["number"]);
        let had = match fs::read_to_string(self.plan_dir(repo, &n).join("input")) {
            Ok(had) => had,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(false),
            Err(_) => return None,
        };
        if !self.list_bound(repo, &n, &had)? {
            return Some(false);
        }
        self.input(repo, issue).map(|input| input == had)
    }
    fn list_bound(&self, repo: &str, n: &str, input: &str) -> Option<bool> {
        match fs::read(self.plan_dir(repo, n).join("tasks.json")) {
            Ok(bytes) => serde_json::from_slice::<List>(&bytes)
                .ok()
                .map(|list| list.source.as_ref().is_none_or(|source| source == input)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(true),
            Err(_) => None,
        }
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
    /// Bind a reconciled list before publishing approval. Receipts belong to
    /// exact tasks, never their position in a later proposal.
    pub(super) fn prepare_plan(&self, repo: &str, n: &str, input: &str, tasks: &[Task]) -> bool {
        let d = self.plan_dir(repo, n);
        if fs::create_dir_all(&d).is_err() {
            return false;
        }
        let mut list: List = match fs::read(d.join("tasks.json")) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(list) => list,
                Err(_) => return false,
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => List {
                source: None,
                revision: 0,
                retained: vec![],
                tasks: vec![],
                children: vec![],
                comment: None,
                rendered: String::new(),
            },
            Err(_) => return false,
        };
        if list.tasks != tasks {
            for (i, child) in list.children.iter_mut().enumerate() {
                child
                    .marker
                    .get_or_insert_with(|| format!("<!-- proj-child {repo}#{n} {} -->", i + 1));
            }
            let mut pool = std::mem::take(&mut list.retained);
            pool.extend(
                std::mem::take(&mut list.tasks)
                    .into_iter()
                    .zip(std::mem::take(&mut list.children))
                    .map(|(task, child)| Retained { task, child }),
            );
            if !pool.is_empty() {
                list.revision += 1;
            }
            list.children = tasks
                .iter()
                .enumerate()
                .map(|(i, task)| {
                    if let Some(at) = pool.iter().position(|old| &old.task == task) {
                        pool.remove(at).child
                    } else {
                        Child {
                            marker: Some(if list.revision == 0 {
                                format!("<!-- proj-child {repo}#{n} {} -->", i + 1)
                            } else {
                                format!(
                                    "<!-- proj-child {repo}#{n} revision-{} {} -->",
                                    list.revision,
                                    i + 1
                                )
                            }),
                            ..Child::default()
                        }
                    }
                })
                .collect();
            list.tasks = tasks.to_vec();
            list.retained = pool;
        }
        list.source = Some(input.to_owned());
        self.save_list(repo, n, &list)
            && (!tasks.is_empty()
                || list.retained.is_empty()
                || self.render_list(repo, n, &mut list))
            && self.keep_planned(repo, n, input).is_ok()
    }

    fn receipt_body(repo: &str, n: &str, i: usize, task: &Task, child: &Child) -> String {
        let body = Self::child_body(repo, n, i, task);
        match &child.marker {
            Some(marker) => {
                format!(
                    "{}{marker}",
                    body.strip_suffix(&format!("<!-- proj-child {repo}#{n} {} -->", i + 1))
                        .unwrap()
                )
            }
            None => body,
        }
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
                source: fs::read_to_string(d.join("input")).ok(),
                revision: 0,
                retained: vec![],
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
        for (i, task) in tasks.iter().enumerate() {
            if list.children[i].number.is_some() {
                continue;
            }
            if let Some(existing) = task.issue {
                if existing.to_string() == n {
                    return false;
                }
                let Some(issue) = api(self.cfg, &format!("repos/{repo}/issues/{existing}")) else {
                    return false;
                };
                if !self.eligible(repo, &issue) || issue.get("pull_request").is_some() {
                    return false;
                }
                list.children[i].number = Some(existing);
                if !self.save_list(repo, n, &list) {
                    return false;
                }
                continue;
            }
            let marker = list.children[i]
                .marker
                .clone()
                .unwrap_or_else(|| format!("<!-- proj-child {repo}#{n} {} -->", i + 1));
            let Some(all) = every(
                self.cfg,
                &format!("repos/{repo}/issues?state=all&per_page=100"),
            ) else {
                return false;
            };
            let matches: Vec<_> = all
                .iter()
                .filter(|issue| {
                    !self.outside(issue)
                        && (self.eligible(repo, issue)
                            || (list.children[i].attempted
                                && list.children[i]
                                    .posting_id
                                    .is_some_and(|id| issue["user"]["id"].as_u64() == Some(id))
                                && issue["title"].as_str() == Some(task.title.as_str())
                                && issue["body"].as_str()
                                    == Some(
                                        Self::receipt_body(repo, n, i, task, &list.children[i])
                                            .as_str(),
                                    )))
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
                // Read the identity using the same repository authentication
                // route as the creation POST, and persist it before sending.
                let Some(id) = crate::github::query_for(
                    self.cfg,
                    repo,
                    "query { viewer { databaseId } }",
                    json!({}),
                )
                .and_then(|v| v["viewer"]["databaseId"].as_u64())
                .filter(|id| *id > 0) else {
                    return false;
                };
                list.children[i].posting_id = Some(id);
                list.children[i].attempted = true;
                if !self.save_list(repo, n, &list) {
                    return false;
                }
                let body = Self::receipt_body(repo, n, i, task, &list.children[i]);
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
        for (i, task) in tasks.iter().enumerate() {
            // The parent already sized this exact generated child. Retain
            // that approval instead of paying a second planner to repeat it.
            let number = list.children[i].number.unwrap().to_string();
            if self.plan_dir(repo, &number).join("input").exists() {
                continue;
            }
            let Some(child) = api(self.cfg, &format!("repos/{repo}/issues/{number}")) else {
                return false;
            };
            let expected = if task.issue.is_some() {
                task.body.clone()
            } else {
                Self::receipt_body(repo, n, i, task, &list.children[i])
            };
            if child["title"].as_str() == Some(task.title.as_str())
                && child["body"].as_str() == Some(expected.as_str())
                && self.eligible(repo, &child)
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
        // Establish durable enrollment before removing the implementation labels.
        // A failed or ambiguous label response leaves the original enrollment
        // intact, so either caller can retry finalization after a crash.
        if !self.add_label(repo, n, super::triage::TRACKER) {
            return false;
        }
        if !self.remove_label(repo, n, super::triage::READY) {
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
        if !self.inbox.planning || self.cfg.paused(repo) {
            return;
        }
        for d in self.plan_dirs(repo) {
            let Some(name) = d.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let Some((_, n)) = name.rsplit_once('-') else {
                continue;
            };
            let Some(parent) = all
                .iter()
                .find(|i| raw(&i["number"]) == n && self.eligible(repo, i))
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
            if list.tasks.is_empty() || self.claimed(repo).contains(n) {
                continue;
            }
            let Some(same) = self.verified_source(repo, parent) else {
                continue;
            };
            if !same {
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
            if !self.eligible(repo, &issue) {
                continue;
            }
            // Apply inherited repair scheduling even when a previous pass already
            // offered this child, or it was reused with an engine label.
            if issue["state"] == "open"
                && self.lane(parent)
                && !self.lane(&issue)
                && !self.add_label(repo, &child.to_string(), &self.inbox.unstick_label)
            {
                self.miss(repo);
                continue;
            }
            let mut advanced = false;
            // A child is done when a merged PR is tied to it, whatever its
            // issue's state: an open one recorded so is closed with it.
            let landed = if issue["state"] == "closed" {
                let Some(landed) = self.landed_child(repo, child) else {
                    self.miss(repo);
                    continue;
                };
                landed
            } else {
                self.recorded_landing(repo, child)
            };
            if let Some(pr) = landed {
                if issue["state"] == "open" && !self.close(repo, &child.to_string(), "completed") {
                    self.miss(repo);
                    continue;
                }
                let waited = list.children[index].waits;
                list.children[index].merged_pr = Some(pr);
                list.children[index].waits = false;
                if !self.save_list(repo, n, &list) {
                    self.retry(repo);
                    continue;
                }
                if waited {
                    self.notify(&[
                        "close",
                        &Self::waits_key(repo, n),
                        "The task list moved on.",
                    ]);
                }
                advanced = true;
            } else if issue["state"] == "closed" {
                // Closed by hand: a no-op, said once on the checklist and to
                // the owner, never left to wait in silence.
                if !list.children[index].waits {
                    list.children[index].waits = true;
                    if !self.save_list(repo, n, &list) {
                        self.retry(repo);
                        continue;
                    }
                    if !self.render_list(repo, n, &mut list) {
                        self.miss(repo);
                    }
                    self.notify(&[
                        "open",
                        &Self::waits_key(repo, n),
                        &format!("{repo}#{n}'s task list waits on #{child}"),
                        &format!(
                            "{repo}#{child} closed without a merged PR tied to it, so the task list on {repo}#{n} waits and its next child doesn't start. If its work landed, record the PR with `proj plan done {repo}#{child} PR-URL`; otherwise reopen it."
                        ),
                    ]);
                }
            } else if !list.children[index].offered
                && self.engine(&issue).is_none()
                && !self.has_task_list(Some(repo), &child.to_string())
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
    fn waits_key(repo: &str, parent: &str) -> String {
        format!("proj-plan-{}-{parent}", key(repo))
    }
    /// Tell the owner (burrow.tools.notify), where there's a notifier.
    fn notify(&self, args: &[&str]) {
        if let Some(notify) = &self.cfg.notify
            && !std::process::Command::new(notify)
                .args(args)
                .status()
                .is_ok_and(|s| s.success())
        {
            eprintln!("proj-inbox: couldn't tell the owner: {}", args.join(" "));
        }
    }
}

#[path = "landed.rs"]
mod landed;
pub(super) use landed::PLAN_USAGE;

#[cfg(test)]
#[path = "plans_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "landed_tests.rs"]
mod landed_tests;

#[cfg(test)]
#[path = "plans_review_tests.rs"]
mod review_tests;

#[cfg(test)]
#[path = "plans_trust_tests.rs"]
mod trust_tests;
