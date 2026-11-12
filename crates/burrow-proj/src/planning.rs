//! Bounded plans are data, never a conversation to resume.
use serde::{Deserialize, Serialize};

pub const MAX_TASKS: usize = 16;
pub const MAX_BODY: usize = 16_000;
pub const MAX_INPUT: usize = 64_000;

/// All turn-start paths, including question defaults, honor the same hold.
pub(crate) fn admit(job: &crate::job::Job) -> Result<(), String> {
    if job.path("planning-hold").exists() || pending_split(job).is_some() {
        return Err(format!("{} is waiting for a bounded task plan", job.id));
    }
    Ok(())
}

/// The per-turn split file is the durable intent, published under the job lock.
/// Admission also checks it if the process dies before the hold/end is written.
pub(crate) fn pending_split(job: &crate::job::Job) -> Option<String> {
    std::fs::read_dir(&job.dir)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".split")?
                .parse::<u64>()
                .ok()
        })
        .max()
        .map(|n| n.to_string())
}

pub(crate) fn declare_split(job: &crate::job::Job, n: &str, tasks: &[Task]) -> std::io::Result<()> {
    crate::swap(&job.turn(n, "split"), &serde_json::to_string(tasks)?)?;
    crate::swap(&job.path("planning-hold"), n)
}

pub fn prompt(input: &str, source: &std::path::Path) -> String {
    if input.len() <= MAX_INPUT {
        return input.to_owned();
    }
    let preview: String = input.chars().take(2000).collect();
    format!(
        "The verified task is larger than the planning prompt budget. Its complete text is in {}. Read it in bounded portions, preserving every authorization boundary and unresolved task. The preview is not sufficient authority for implementation. Plan smaller children; do not carry this workstream in a session.\n\nPreview:\n{preview}",
        source.display()
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue: Option<u64>,
    pub title: String,
    pub body: String,
    pub check: String,
}

/// A planner returns JSON after `split`, without executable operations.
/// Validate the entire plan before creating even its first child.
pub fn parse(text: &str) -> Result<Vec<Task>, String> {
    let tasks: Vec<Task> = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if tasks.len() < 2 || tasks.len() > MAX_TASKS {
        return Err(format!("a split needs 2..={MAX_TASKS} tasks"));
    }
    for (i, t) in tasks.iter().enumerate() {
        if t.title.trim().is_empty()
            || t.title.chars().count() > 200
            || t.title.contains(['\n', '\r'])
            || t.body.trim().is_empty()
            || t.check.trim().is_empty()
            || t.body.len() + t.check.len() > MAX_BODY
        {
            return Err(format!(
                "task {} needs a bounded title, body and check",
                i + 1
            ));
        }
        if tasks[..i].iter().any(|other| other.title == t.title) {
            return Err(format!("duplicate task title: {}", t.title));
        }
        if t.issue == Some(0)
            || t.issue
                .is_some_and(|n| tasks[..i].iter().any(|other| other.issue == Some(n)))
        {
            return Err("existing child issue numbers must be positive and unique".into());
        }
    }
    Ok(tasks)
}

pub const INSTRUCTIONS: &str = "Plan the size of this task before implementation. Read the verified task and repository, including existing issues and open PRs, but no earlier sessions' transcripts. Change nothing. Answer `ready` if this is one self-contained PR that a fresh session can finish and check. A known GitHub dependency can be awaited in that fresh session; it is not on its own a reason for `later`. For a lane, epic, tracker or bigger task, answer `split` on the first line, followed by a JSON array of 2 to 16 objects with title, body and check. Reuse existing authorized child issues rather than duplicate their work: optionally include issue (its positive number in this repository), copying its title and body verbatim for size approval. Each child must be self-contained, one PR, with concrete verification, the original authorization boundaries and necessary task context. Put children in dependency order. Do not expand the original authority. Never preserve a lane as a child or ask one session to carry a workstream. If there are more than 16 tasks, make bounded planning children for the remaining groups. Do not include Markdown fences. Use `owner` only for an actual decision reserved to the owner, and `later` only for a missing prerequisite you cannot resolve.";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_planning_hold_blocks_every_turn_start_before_any_question_or_prompt_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: crate::config::Config =
            serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = tmp.path().join("jobs");
        let job = crate::job::Job::new(&cfg, "held");
        std::fs::create_dir_all(&job.dir).unwrap();
        std::fs::write(job.path("planning-hold"), "pending").unwrap();
        for answered in [false, true] {
            let result = if answered {
                crate::turn::start_answer(&cfg, "held", "new answer", false)
            } else {
                crate::turn::start(&cfg, "held", "watcher news", false)
            };
            assert!(result.unwrap_err().contains("bounded task plan"));
            assert!(!job.path("turns").exists());
            assert!(!job.path("prompt.1").exists());
        }
    }
    #[test]
    fn only_whole_bounded_self_contained_plans_are_accepted() {
        let one = Task {
            issue: None,
            title: "first".into(),
            body: "bounded task".into(),
            check: "regression test".into(),
        };
        let two = Task {
            title: "second".into(),
            ..one.clone()
        };
        let plan = serde_json::to_string(&vec![one.clone(), two.clone()]).unwrap();
        assert_eq!(parse(&plan).unwrap(), vec![one.clone(), two.clone()]);
        for bad in [
            vec![],
            vec![one.clone()],
            vec![one.clone(); 2],
            vec![one.clone(); 17],
            vec![
                one.clone(),
                Task {
                    body: "x".repeat(MAX_BODY),
                    ..two.clone()
                },
            ],
            vec![
                one,
                Task {
                    check: String::new(),
                    ..two
                },
            ],
        ] {
            assert!(parse(&serde_json::to_string(&bad).unwrap()).is_err());
        }
        assert!(
            parse("[{\"title\":\"x\",\"body\":\"y\",\"check\":\"z\",\"shell\":\"bad\"}]").is_err()
        );
    }
    #[test]
    fn a_growing_parent_never_grows_the_planning_prompt_without_bound() {
        for size in [100, MAX_INPUT, MAX_INPUT * 20] {
            let text = prompt(
                &"x".repeat(size),
                std::path::Path::new("/state/original.txt"),
            );
            assert!(text.len() <= MAX_INPUT);
            if size > MAX_INPUT {
                assert!(text.contains("/state/original.txt"));
            }
        }
    }
    #[test]
    fn every_planning_job_has_a_fresh_session_and_keeps_its_complete_source() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: crate::config::Config =
            serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = tmp.path().join("jobs");
        cfg.projects = tmp.path().join("projects");
        cfg.scratch = tmp.path().join("scratch");
        cfg.waits = None;
        cfg.tools.systemd_run = "true".into();
        cfg.tools.systemctl = "false".into();
        std::fs::create_dir_all(cfg.projects.join("project")).unwrap();
        let mut previous = Vec::new();
        for i in 1..=3 {
            let source = format!(
                "Original task. Authorization: no deletion. Unresolved PR #4. {}",
                "work ".repeat(MAX_INPUT * i)
            );
            let given = prompt(&source, std::path::Path::new("@PROJ_TASK_SOURCE@"));
            let new = crate::cmd::New {
                task_source: Some(source.clone()),
                engine: Some(crate::engine::Engine::Codex),
                local_work: true,
                ..Default::default()
            };
            let id = crate::cmd::start_job(&cfg, &new, "project", &given).unwrap();
            let job = crate::job::Job::new(&cfg, &id);
            assert!(!previous.contains(&id));
            assert!(!job.path("session").exists());
            assert_eq!(
                std::fs::read_to_string(job.path("task-source")).unwrap(),
                source
            );
            let actual = std::fs::read_to_string(job.path("prompt.1")).unwrap();
            assert!(actual.len() < MAX_INPUT);
            assert!(actual.contains(&job.path("task-source").to_string_lossy().to_string()));
            assert_eq!(crate::read(&job.path("engine")).as_deref(), Some("codex"));
            previous.push(id);
        }
    }
    #[test]
    fn split_intent_and_admission_serialize_even_before_the_hold_is_published() {
        let tmp = tempfile::tempdir().unwrap();
        let mut cfg: crate::config::Config =
            serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = tmp.path().join("jobs");
        let job = crate::job::Job::new(&cfg, "split");
        std::fs::create_dir_all(&job.dir).unwrap();
        std::fs::write(job.path("turns"), "1").unwrap();
        let held = job.lock().unwrap();
        let (send, recv) = std::sync::mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                let _held = job.lock().unwrap();
                send.send(crate::turn::start_answer(
                    &cfg,
                    "split",
                    "concurrent reply",
                    false,
                ))
                .unwrap();
            });
            assert!(recv.try_recv().is_err());
            std::fs::write(job.turn("1", "split"), "[]").unwrap();
            drop(held);
            assert!(
                recv.recv()
                    .unwrap()
                    .unwrap_err()
                    .contains("bounded task plan")
            );
        });
        assert!(!job.path("prompt.2").exists());
        assert_eq!(job.turns().as_deref(), Some("1"));
        assert!(crate::turn::start(&cfg, "split", "watcher resume", false).is_err());
    }
}
