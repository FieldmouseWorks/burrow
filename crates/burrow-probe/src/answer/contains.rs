//! An ancestry verdict uses the running input (or, for the machine's own
//! repo, configuration) revision, never a checkout's HEAD.
use super::Machine;
use serde_json::Value;
use std::fs;

pub(super) fn answer(m: &Machine, repo: &str, commit: &str) -> Result<String, String> {
    let is_system = m
        .cfg
        .system_repo
        .as_deref()
        .is_some_and(|r| r.eq_ignore_ascii_case(repo));
    // The inputs manifest is optional; a missing one only matters to a repo
    // that has no other source of a running revision.
    let inputs: Result<Value, String> = fs::read_to_string(m.path("/etc/burrow/inputs.json"))
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
        .and_then(|v: Value| {
            if v.is_object() {
                Ok(v)
            } else {
                Err("invalid running inputs".to_owned())
            }
        });
    let mut revisions: Vec<String> = Vec::new();
    match &inputs {
        Ok(inputs) => revisions.extend(
            inputs
                .as_object()
                .into_iter()
                .flat_map(|o| o.values())
                .filter(|i| {
                    i["repo"]
                        .as_str()
                        .is_some_and(|r| r.eq_ignore_ascii_case(repo))
                })
                .filter_map(|i| i["rev"].as_str().map(str::to_owned)),
        ),
        Err(e) if !is_system => return Err(e.clone()),
        Err(_) => {}
    }
    // The machine's own repo is the configuration, not an input.
    if is_system {
        revisions.push(super::system::configuration_revision(m)?);
    }
    if revisions.is_empty() {
        return Err(format!(
            "{repo} is neither the machine's repo nor one of the running system's inputs"
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
            let mut words = vec![
                "--no-lazy-fetch",
                "--no-replace-objects",
                "-c",
                "core.commitGraph=false",
                "-C",
                dir,
            ];
            words.extend_from_slice(args);
            m.run.history(git, &words, m.uid)
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
            let rev = rev.as_str();
            if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err("invalid running revision".into());
            }
            // Object disambiguation never consults hexadecimal branch or tag names.
            let resolve = |hash: &str| -> Result<String, String> {
                let found = history(&["rev-parse", &format!("--disambiguate={hash}")])?;
                let objects: Vec<_> = found.stdout.lines().collect();
                if found.code != Some(0)
                    || objects.len() != 1
                    || objects[0].len() != 40
                    || !objects[0].bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(format!(
                        "history for {hash} is unavailable or ambiguous in {repo}"
                    ));
                }
                let object = objects[0];
                let kind = history(&["cat-file", "-t", object])?;
                if kind.code != Some(0) || kind.stdout.trim() != "commit" {
                    return Err(format!(
                        "{hash} is not an unambiguous commit object in {repo}"
                    ));
                }
                Ok(object.to_owned())
            };
            let commit_id = resolve(commit)?;
            let running_id = resolve(rev)?;
            let verdict = history(&["merge-base", "--is-ancestor", &commit_id, &running_id])?;
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
        ambiguous: bool,
        calls: RefCell<Vec<Vec<String>>>,
    }
    impl Run for History {
        fn run(&self, _: &Path, args: &[&str], _: bool, _: u32) -> Result<Ran, String> {
            self.calls
                .borrow_mut()
                .push(args.iter().map(|s| s.to_string()).collect());
            assert_eq!(&args[..2], &["--no-lazy-fetch", "--no-replace-objects"]);
            assert_eq!(&args[2..4], &["-c", "core.commitGraph=false"]);
            let (code, stdout) = match args[6] {
                "remote" => (0, "https://github.com/sam/tool.git"),
                "merge-base" => (self.code, ""),
                "rev-parse" if args[7] == "--is-shallow-repository" => {
                    (0, if self.shallow { "true" } else { "false" })
                }
                "rev-parse" => {
                    if self.missing {
                        (128, "")
                    } else {
                        let prefix = args[7].strip_prefix("--disambiguate=").unwrap();
                        return Ok(Ran {
                            code: Some(0),
                            stdout: format!(
                                "{}\n{}",
                                if prefix.len() == 40 {
                                    prefix.to_owned()
                                } else {
                                    "a".repeat(40)
                                },
                                if self.ambiguous {
                                    format!("{}\n", "c".repeat(40))
                                } else {
                                    String::new()
                                }
                            ),
                            stderr: String::new(),
                        });
                    }
                }
                "cat-file" => (0, "commit"),
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
    fn real_history_ignores_grafts_and_caches() {
        real_history(false);
    }
    #[test]
    fn real_history_resolves_object_prefixes_without_hexadecimal_refs() {
        real_history(true);
    }
    #[test]
    fn history_environment_is_host_owned() {
        if std::env::var_os("BURROW_HISTORY_CHILD").is_none() {
            let out = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "answer::contains::tests::history_environment_is_host_owned",
                ])
                .env("BURROW_HISTORY_CHILD", "1")
                .env("GIT_DIR", "/untrusted")
                .env("GIT_GRAFT_FILE", "/untrusted/grafts")
                .env("GIT_CONFIG_PARAMETERS", "'core.commitGraph=true'")
                .env("GIT_CONFIG_COUNT", "1")
                .env("GIT_CONFIG_KEY_0", "core.commitGraph")
                .env("GIT_CONFIG_VALUE_0", "true")
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            return;
        }
        let got = crate::answer::Commands
            .history(Path::new("env"), &[], 1001)
            .unwrap();
        let git: Vec<_> = got
            .stdout
            .lines()
            .filter(|s| s.starts_with("GIT_"))
            .collect();
        assert_eq!(git.len(), 4, "{git:?}");
        for expected in [
            "GIT_CONFIG_NOSYSTEM=1",
            "GIT_CONFIG_GLOBAL=/dev/null",
            "GIT_NO_REPLACE_OBJECTS=1",
            "GIT_GRAFT_FILE=/dev/null",
        ] {
            assert!(git.contains(&expected), "{git:?}");
        }
    }
    fn real_history(refs_only: bool) {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repo = root.join("projects/tool");
        fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "Test"]);
        git(&["remote", "add", "origin", "https://github.com/sam/tool.git"]);
        git(&["commit", "--allow-empty", "-qm", "first"]);
        let first = git(&["rev-parse", "HEAD"]);
        git(&["commit", "--allow-empty", "-qm", "second"]);
        let second = git(&["rev-parse", "HEAD"]);
        fs::create_dir_all(root.join("etc/burrow")).unwrap();
        let cfg: Config = serde_json::from_value(serde_json::json!({"repo":"sam/host","label":"probe","authors":[],"host":"host","user":"dev","stateDir":root,"projects":root.join("projects"),"proj":null,"journal":{"units":[],"userUnits":[]},"tools":{"journalctl":"false","systemctl":"false"}})).unwrap();
        let commands = crate::answer::Commands;
        let machine = Machine {
            cfg: &cfg,
            root: root.to_owned(),
            now: 0,
            uid: 1001,
            run: &commands,
        };
        let running = |rev: &str| {
            fs::write(
                root.join("etc/burrow/inputs.json"),
                serde_json::json!({"alias":{"repo":"sam/tool","rev":rev}}).to_string(),
            )
            .unwrap()
        };
        running(&second);
        if !refs_only {
            git(&["commit-graph", "write", "--reachable"]);
            fs::write(repo.join(".git/info/grafts"), format!("{second}\n")).unwrap();
            assert!(
                answer(&machine, "sam/tool", &first)
                    .unwrap()
                    .ends_with(": yes\n")
            );
            fs::write(
                repo.join(".git/info/grafts"),
                format!("{first} {second}\n{second}\n"),
            )
            .unwrap();
            running(&first);
            assert!(
                answer(&machine, "sam/tool", &second)
                    .unwrap()
                    .ends_with(": no\n")
            );
        }
        let _ = fs::remove_file(repo.join(".git/info/grafts"));
        running(&first);
        git(&["branch", "deadbee", &first]);
        assert!(answer(&machine, "sam/tool", "deadbee").is_err());
        // A hexadecimal ref must not shadow a real object prefix either.
        git(&["branch", &second[..10], &first]);
        assert!(
            answer(&machine, "sam/tool", &second[..10])
                .unwrap()
                .ends_with(": no\n")
        );
    }
    #[test]
    fn host_repo_is_answered_from_the_configuration_revision() {
        use std::process::Command;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let repo = root.join("projects/host");
        fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "{args:?}: {:?}", out.stderr);
            String::from_utf8(out.stdout).unwrap().trim().to_owned()
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "Test"]);
        git(&["remote", "add", "origin", "https://github.com/sam/host.git"]);
        git(&["commit", "--allow-empty", "-qm", "first"]);
        let first = git(&["rev-parse", "HEAD"]);
        git(&["commit", "--allow-empty", "-qm", "second"]);
        let second = git(&["rev-parse", "HEAD"]);
        fs::create_dir_all(root.join("etc/burrow")).unwrap();
        let bin = root.join("run/current-system/sw/bin");
        fs::create_dir_all(&bin).unwrap();
        let script = bin.join("nixos-version");
        fs::write(
            &script,
            format!("#!/bin/sh\necho '{{\"nixosVersion\":\"1\",\"configurationRevision\":\"{first}\"}}'\n"),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let cfg: Config = serde_json::from_value(serde_json::json!({"repo":"sam/issues","systemRepo":"sam/host","label":"probe","authors":[],"host":"host","user":"dev","stateDir":root,"projects":root.join("projects"),"proj":null,"journal":{"units":[],"userUnits":[]},"tools":{"journalctl":"false","systemctl":"false"}})).unwrap();
        let commands = crate::answer::Commands;
        let machine = Machine {
            cfg: &cfg,
            root: root.to_owned(),
            now: 0,
            uid: 1001,
            run: &commands,
        };
        // Running at `first`: first is an ancestor, second is not.
        assert!(
            answer(&machine, "sam/host", &first)
                .unwrap()
                .ends_with(": yes\n")
        );
        assert!(
            answer(&machine, "sam/host", &second)
                .unwrap()
                .ends_with(": no\n")
        );
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
        for (code, shallow, missing, ambiguous, expected) in [
            (0, false, false, false, Some("yes")),
            (1, false, false, false, Some("no")),
            (1, true, false, false, None),
            (0, false, true, false, None),
            (128, false, false, false, None),
            (0, false, false, true, None),
        ] {
            let history = History {
                code,
                shallow,
                missing,
                ambiguous,
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
                .filter(|a| a[6] == "merge-base")
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
