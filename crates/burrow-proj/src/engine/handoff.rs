//! A turn whose conversation hasn't seen all of the job. Each engine resumes
//! its own conversation, so a turn that starts without one (lost, or the job
//! ran elsewhere) is first given what it missed: every turn since its
//! conversation's last (all of them, when it has none), each with what it
//! was asked (the brief, a reply, a watcher's news), always whole, and what
//! it answered. Its work in the repo is where those turns left it, and each
//! turn's transcript is named, for whatever its answer leaves out.

#[cfg(test)]
use std::fs;
use std::path::PathBuf;

use super::stream::resume;
use super::{Engine, answer, stream_names_session};
use crate::record::TurnFile;
use crate::{job::Job, read};

/// How much of one answer is passed on: a long one is cut in its middle,
/// which keeps how it starts and how it ends, and says where all of it is.
/// What a turn was asked is never cut: it's the task.
const KEEP: usize = 16_000;

/// The engine turn `m` ran on. One from before engines were chosen, or on
/// one since retired, counts as Claude's: its stream names no session Claude
/// can resume, so it's handed on all the same.
fn engine_of(job: &Job, m: u64) -> Engine {
    read(&job.turn_file(&m.to_string(), TurnFile::Engine))
        .and_then(|e| Engine::parse(&e).ok())
        .unwrap_or(Engine::Claude)
}

/// The turns before turn `n` that `engine`'s conversation hasn't seen, oldest
/// first: the ones after the last that ran in it, or all of them when it has
/// no conversation to resume.
fn unseen(job: &Job, n: u64, engine: Engine) -> Vec<u64> {
    let saw = |m: &u64| {
        engine_of(job, *m).conversation() == engine.conversation()
            && stream_names_session(&job.turn_file(&m.to_string(), TurnFile::Jsonl), 0)
    };
    let from = if resume(job, engine).is_some() {
        (1..n).rev().find(saw).map_or(1, |m| m + 1)
    } else {
        1
    };
    (from..n).collect()
}

/// `text`, cut in its middle past [`KEEP`], with `whole`, how to read all
/// of it, where it's cut.
fn cut(text: &str, whole: &str) -> String {
    let chars: Vec<char> = text.trim().chars().collect();
    if chars.len() <= KEEP {
        return chars.into_iter().collect();
    }
    let half = KEEP / 2;
    let head: String = chars[..half].iter().collect();
    let tail: String = chars[chars.len() - half..].iter().collect();
    format!(
        "{head}\n\n[… {} characters cut; all of it: {whole} …]\n\n{tail}",
        chars.len() - KEEP
    )
}

/// The prompt turn `n` starts with on `engine`: its own `prompt.N`, or, when
/// that engine's conversation missed some of the job, `N.handoff`, those
/// turns and then this one's prompt.
pub fn first_prompt(job: &Job, n: &str, engine: Engine) -> std::io::Result<PathBuf> {
    let prompt = job.prompt(n);
    let Ok(number) = n.parse::<u64>() else {
        return Ok(prompt);
    };
    let missed = unseen(job, number, engine);
    if missed.is_empty() {
        return Ok(prompt);
    }
    let mut text = if resume(job, engine).is_some() {
        "This job went on without this conversation since you last saw it. Here is what each turn since was asked and answered, oldest first; its work (files, commits, branches, PRs) is where they left it.".to_owned()
    } else {
        "This job began before this conversation. Here is what each turn so far was asked and answered, oldest first, starting with its brief; its work (files, commits, branches, PRs) is where they left it.".to_owned()
    };
    for m in missed {
        let m = m.to_string();
        let asked = read(&job.prompt(&m)).unwrap_or_default();
        let transcript = job.turn_file(&m, TurnFile::Jsonl);
        let said = answer(&transcript);
        let said = if said.trim().is_empty() {
            "(nothing: the turn ended without an answer)".to_owned()
        } else {
            cut(&said, &format!("`proj log {} --turn {m}`", job.id))
        };
        text.push_str(&format!(
            "\n\n## Turn {m} ({}), asked\n\n{}\n\n## Turn {m}, answered\n\n{said}\n\n(Everything turn {m} did: {}.)",
            engine_of(job, m.parse().unwrap_or(0)).name(),
            asked.trim(),
            transcript.display(),
        ));
    }
    let asked = String::from_utf8_lossy(&crate::state_reads::bytes(&prompt)?).into_owned();
    text.push_str(&format!("\n\n## This turn, asked\n\n{asked}\n"));
    let handoff = job.turn_file(n, TurnFile::Handoff);
    crate::state_reads::write(&handoff, &text)?;
    Ok(handoff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    struct Turns {
        _dir: tempfile::TempDir,
        cfg: Config,
    }

    impl Turns {
        fn new() -> Turns {
            let dir = tempfile::tempdir().unwrap();
            let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
            cfg.jobs = dir.path().join("jobs");
            fs::create_dir_all(cfg.jobs.join("job")).unwrap();
            Turns { _dir: dir, cfg }
        }

        fn job(&self) -> Job<'_> {
            Job::new(&self.cfg, "job")
        }

        /// Turn `n` ran on `engine` (None: before engines were chosen),
        /// asked `asked`, in its conversation, and answered `said`.
        fn ran(&self, n: u64, engine: Option<Engine>, asked: &str, said: &str) {
            let job = self.job();
            let n = n.to_string();
            fs::write(job.path(&format!("prompt.{n}")), format!("{asked}\n")).unwrap();
            let engine_name = engine.unwrap_or(Engine::Claude).name();
            if let Some(e) = engine {
                fs::write(job.turn(&n, "engine"), e.name()).unwrap();
            }
            let stream = format!(
                "{}\n{}\n",
                serde_json::json!({"type": "system", "session_id": "claude-session"}),
                serde_json::json!({"type": "result", "is_error": false, "result": said}),
            );
            fs::write(job.turn(&n, "jsonl"), stream).unwrap();
            fs::write(
                job.path(&format!("session.{engine_name}")),
                "claude-session\n",
            )
            .unwrap();
            fs::write(job.path("session.engine"), engine_name).unwrap();
        }

        /// Turn `n` is about to start on `engine`, asked `asked`: what it's given.
        fn starts(&self, n: u64, engine: Engine, asked: &str) -> String {
            let job = self.job();
            let n = n.to_string();
            fs::write(job.path(&format!("prompt.{n}")), format!("{asked}\n")).unwrap();
            fs::write(job.turn(&n, "engine"), engine.name()).unwrap();
            fs::read_to_string(first_prompt(&job, &n, engine).unwrap()).unwrap()
        }
    }

    /// The subscription and the credit run the same Claude Code on the same
    /// sessions: a job moving between them carries on in its conversation,
    /// handed nothing it already saw.
    #[test]
    fn the_claude_engines_share_one_conversation() {
        let t = Turns::new();
        t.ran(1, Some(Engine::Claude), "the brief", "first answer");
        assert_eq!(
            resume(&t.job(), Engine::ClaudeApi).as_deref(),
            Some("claude-session")
        );
        assert_eq!(t.starts(2, Engine::ClaudeApi, "a reply"), "a reply\n");
        // Its stream names the session as Claude's (stream.rs), so the next
        // turn back on the subscription has seen it too.
        let job = t.job();
        let stream = format!(
            "{}\n{}\n",
            serde_json::json!({"type": "system", "session_id": "claude-session"}),
            serde_json::json!({"type": "result", "is_error": false, "result": "second"}),
        );
        fs::write(job.turn("2", "jsonl"), stream).unwrap();
        assert_eq!(t.starts(3, Engine::Claude, "carry on"), "carry on\n");
    }

    #[test]
    fn the_same_engine_carries_on_in_its_own_conversation() {
        let t = Turns::new();
        assert_eq!(t.starts(1, Engine::Claude, "the brief"), "the brief\n");
        t.ran(1, Some(Engine::Claude), "the brief", "first answer");
        assert_eq!(t.starts(2, Engine::Claude, "a reply"), "a reply\n");
    }

    #[test]
    fn a_lost_conversation_gets_the_brief_and_everything_since() {
        let t = t_lost(&[
            ("the brief", "started on it"),
            ("the owner says: use the other crate", "switched crates"),
        ]);
        let given = t.starts(
            3,
            Engine::Claude,
            "PR #9 changed: checks finished. Carry on.",
        );
        assert!(
            given.starts_with("This job began before this conversation"),
            "{given}"
        );
        for part in [
            "## Turn 1 (",
            "the brief",
            "started on it",
            "use the other crate",
            "switched crates",
            "## This turn, asked\n\nPR #9 changed",
        ] {
            assert!(given.contains(part), "{part} missing from {given}");
        }
        assert!(given.find("the brief") < given.find("use the other crate"));
    }

    /// Turns that ran, each asked and answered, whose conversation is gone.
    fn t_lost(turns: &[(&str, &str)]) -> Turns {
        let t = Turns::new();
        for (n, (asked, said)) in turns.iter().enumerate() {
            t.ran(n as u64 + 1, Some(Engine::Claude), asked, said);
        }
        let job = t.job();
        fs::remove_file(job.path("session.claude")).unwrap();
        fs::remove_file(job.path("session.engine")).unwrap();
        t
    }

    #[test]
    fn turns_from_before_engines_were_claudes() {
        let t = Turns::new();
        t.ran(1, None, "the brief", "an old answer");
        assert_eq!(t.starts(2, Engine::Claude, "a reply"), "a reply\n");
    }

    #[test]
    fn a_long_answer_keeps_its_start_and_end_and_says_where_the_rest_is() {
        let long = format!("start{}end", "x".repeat(KEEP * 2));
        let kept = cut(&long, "`proj log job --turn 1`");
        assert!(kept.starts_with("start") && kept.ends_with("end"));
        assert!(
            kept.contains("all of it: `proj log job --turn 1`"),
            "{kept}"
        );
        assert!(kept.len() < KEEP + 200);
    }

    /// What a turn was asked is the task: never cut, however long, in
    /// either direction, the brief and every reply since.
    #[test]
    fn every_instruction_is_passed_on_whole() {
        let long = |what: &str| {
            format!(
                "{what} begins.\n{}\nIn its middle: {what} must keep the old API.\n{}\n{what} ends.",
                "context ".repeat(KEEP / 4),
                "more ".repeat(KEEP / 4),
            )
        };
        {
            let said = format!("began{}done", "y".repeat(KEEP * 2));
            let t = t_lost(&[(&long("the brief"), &said), (&long("the reply"), "replied")]);
            let given = t.starts(3, Engine::Claude, "carry on");
            for what in ["the brief", "the reply"] {
                assert!(given.contains(&long(what)), "{what} cut from the handoff");
            }
            // The long answer is cut, and says how to read all of it, and
            // each turn names its transcript.
            assert!(!given.contains(&said));
            assert!(given.contains("all of it: `proj log job --turn 1`"));
            for m in ["1", "2"] {
                let transcript = t.job().turn(m, "jsonl");
                assert!(
                    given.contains(&format!(
                        "Everything turn {m} did: {}.",
                        transcript.display()
                    )),
                    "turn {m}'s transcript isn't named"
                );
            }
        }
    }
}
