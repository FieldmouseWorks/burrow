//! An ancestry verdict uses the running input revision, never a checkout's HEAD.
use super::Machine;
use serde_json::Value;
use std::fs;

pub(super) fn answer(m: &Machine, repo: &str, commit: &str) -> Result<String, String> {
    let inputs: Value = serde_json::from_str(
        &fs::read_to_string(m.path("/etc/burrow/inputs.json")).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let revisions: Vec<&str> = inputs
        .as_object()
        .ok_or("invalid running inputs")?
        .values()
        .filter(|i| {
            i["repo"]
                .as_str()
                .is_some_and(|r| r.eq_ignore_ascii_case(repo))
        })
        .filter_map(|i| i["rev"].as_str())
        .collect();
    if revisions.is_empty() {
        return Err(format!(
            "{repo} is not identified in the running system's inputs"
        ));
    }
    let projects = m
        .cfg
        .projects
        .as_ref()
        .ok_or("no project histories configured")?;
    let git = m.cfg.tools.git.as_path();
    for entry in fs::read_dir(projects).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        let Some(dir) = path.to_str() else { continue };
        let history = |args: &[&str]| {
            let mut words = vec!["--no-lazy-fetch", "--no-replace-objects", "-C", dir];
            words.extend_from_slice(args);
            m.run.run(git, &words, false, m.uid)
        };
        let remote = history(&["remote", "get-url", "origin"])?;
        let origin = remote.stdout.trim().trim_end_matches(".git");
        if ![
            format!("https://github.com/{repo}"),
            format!("git@github.com:{repo}"),
            format!("ssh://git@github.com/{repo}"),
        ]
        .iter()
        .any(|r| r.eq_ignore_ascii_case(origin))
        {
            continue;
        }
        let mut out = String::new();
        for rev in &revisions {
            if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid running revision".into());
            }
            // Missing or ambiguous history is unknown, never a false negative.
            for hash in [commit, *rev] {
                let found = history(&["rev-parse", "--verify", &format!("{hash}^{{commit}}")])?;
                if found.code != Some(0) {
                    return Err(format!(
                        "history for {hash} is unavailable or ambiguous in {repo}"
                    ));
                }
            }
            let verdict = history(&["merge-base", "--is-ancestor", commit, rev])?;
            let verdict = match verdict.code {
                Some(0) => "yes",
                Some(1) => {
                    let shallow = history(&["rev-parse", "--is-shallow-repository"])?;
                    if shallow.code != Some(0) || shallow.stdout.trim() != "false" {
                        return Err("complete ancestry is unavailable in this shallow or unreadable history".into());
                    }
                    "no"
                }
                _ => return Err("git could not establish ancestry".into()),
            };
            out.push_str(&format!(
                "{repo} running {rev} contains {commit}: {verdict}\n"
            ));
        }
        return Ok(out);
    }
    Err(format!("no local history for {repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Config,
        answer::{Machine, Ran, Run},
        query::Query,
    };
    use std::{cell::RefCell, path::Path};
    struct History {
        code: i32,
        shallow: bool,
        missing: bool,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl Run for History {
        fn run(&self, _: &Path, args: &[&str], _: bool, _: u32) -> Result<Ran, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            assert_eq!(&args[..2], &["--no-lazy-fetch", "--no-replace-objects"]);
            let (code, stdout) = match args[4] {
                "remote" => (0, "https://github.com/sam/tool.git"),
                "merge-base" => (self.code, ""),
                "rev-parse" if args[5] == "--is-shallow-repository" => {
                    (0, if self.shallow { "true" } else { "false" })
                }
                "rev-parse" => (if self.missing { 128 } else { 0 }, ""),
                _ => panic!("unexpected {args:?}"),
            };
            Ok(Ran {
                code: Some(code),
                stdout: stdout.into(),
                stderr: String::new(),
            })
        }
    }
    #[test]
    fn ancestry_is_of_the_running_input_and_missing_history_is_never_no() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("etc/burrow")).unwrap();
        fs::create_dir_all(root.join("projects/tool")).unwrap();
        let rev = "b".repeat(40);
        fs::write(
            root.join("etc/burrow/inputs.json"),
            serde_json::json!({"aliased-input":{"repo":"sam/tool","rev":rev}}).to_string(),
        )
        .unwrap();
        let cfg: Config = serde_json::from_value(serde_json::json!({"repo":"sam/host","label":"probe","authors":[],"host":"host","user":"dev","stateDir":root,"projects":root.join("projects"),"proj":null,"journal":{"units":[],"userUnits":[]},"tools":{"journalctl":"false","systemctl":"false"}})).unwrap();
        for (code, shallow, missing, expected) in [
            (0, false, false, Some("yes")),
            (1, false, false, Some("no")),
            (1, true, false, None),
            (0, false, true, None),
            (128, false, false, None),
        ] {
            let history = History {
                code,
                shallow,
                missing,
                calls: RefCell::new(Vec::new()),
            };
            let machine = Machine {
                cfg: &cfg,
                root: root.to_owned(),
                now: 0,
                uid: 1001,
                run: &history,
            };
            let got = machine.answer(&Query::parse("contains sam/tool aaaaaaa").unwrap());
            if let Some(expected) = expected {
                assert!(got.unwrap().ends_with(&format!(": {expected}\n")));
            } else {
                assert!(got.is_err());
            }
            for call in history
                .calls
                .borrow()
                .iter()
                .filter(|a| a[4] == "merge-base")
            {
                assert_eq!(call.last().unwrap(), &rev);
                assert!(!call.iter().any(|s| s == "HEAD"));
            }
        }
        for invalid in [
            "contains sam/tool HEAD",
            "contains ../tool aaaaaaa",
            "contains sam/tool --help",
            "contains sam/tool aaaaaaa extra",
        ] {
            assert!(Query::parse(invalid).is_err());
        }
    }
}
