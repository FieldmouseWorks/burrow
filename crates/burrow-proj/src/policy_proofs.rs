//! Vacate proof cgroups using their durable, repository-bound build records.
use crate::{config::Config, swap, systemd};
use serde_json::{Value, json};
use std::{fs, process::Command};
pub(crate) fn reconcile(cfg: &Config) -> Result<(), String> {
    let Some(root) = &cfg.proofs_state else {
        return Ok(());
    };
    let entries = match fs::read_dir(root) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.to_string()),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let record = dir.join("build.json");
        if !record.exists() {
            continue;
        }
        let b: Value = serde_json::from_slice(&fs::read(&record).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let repo = b["repo"]
            .as_str()
            .ok_or("proof lacks repository identity")?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("proof-")
            || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err("invalid proof unit identity".into());
        }
        let unit = format!("autoland-{name}.service");
        let mark = dir.join("policy-paused");
        if dir.join("policy-restoring").exists() && systemd::gone(cfg, &unit) {
            fs::rename(dir.join("policy-restoring"), &mark).map_err(|e| e.to_string())?;
        }
        if cfg.paused(repo) {
            if !systemd::gone(cfg, &unit) {
                let env = systemd::show(cfg, &unit, "Environment")
                    .split_whitespace()
                    .map(|s| s.trim_matches('"'))
                    .filter(|s| s.starts_with("PATH=") || s.starts_with("AUTOLAND_CONFIG="))
                    .collect::<Vec<_>>()
                    .join(" ");
                swap(
                    &dir.join("policy-env.json"),
                    &json!({"environment":env}).to_string(),
                )
                .map_err(|e| e.to_string())?;
                swap(&mark, "paused").map_err(|e| e.to_string())?;
                super::policy::kill(cfg, &unit)?;
            }
        } else if mark.exists() && systemd::gone(cfg, &unit) {
            let _ = fs::remove_file(dir.join("end.json"));
            fs::rename(&mark, dir.join("policy-restoring")).map_err(|e| e.to_string())?;
            let ok = if b.get("runs").is_some() {
                cfg.proof_run.as_ref().is_some_and(|p| {
                    Command::new(p)
                        .arg("start")
                        .arg(&dir)
                        .status()
                        .is_ok_and(|s| s.success())
                })
            } else {
                let Some((program, args)) = cfg.proof_build_driver.split_first() else {
                    return Err("proof build driver is not configured".into());
                };
                let saved: Value = serde_json::from_slice(
                    &fs::read(dir.join("policy-env.json")).map_err(|e| e.to_string())?,
                )
                .map_err(|e| e.to_string())?;
                let mut cmd = systemd::systemd_run(cfg);
                cmd.args(["--quiet", "--collect"])
                    .arg(format!("--unit={unit}"));
                // The saved PATH keeps the proof's tools; its saved
                // AUTOLAND_CONFIG is an earlier deployment's admission
                // policy, so the current one replaces it.
                for word in saved["environment"]
                    .as_str()
                    .unwrap_or("")
                    .split_whitespace()
                {
                    let word = word.trim_matches('"');
                    if word.starts_with("PATH=") {
                        cmd.arg(format!("--setenv={word}"));
                    }
                }
                let Some(current) = &cfg.proof_driver_config else {
                    return Err("proof build driver's current config is not set".into());
                };
                cmd.arg(format!("--setenv=AUTOLAND_CONFIG={}", current.display()));
                cmd.arg("--")
                    .arg(program)
                    .args(args)
                    .args(["build", &name])
                    .status()
                    .is_ok_and(|s| s.success())
            };
            if !ok {
                let _ = fs::rename(dir.join("policy-restoring"), &mark);
                return Err(format!("could not resume proof {name}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restored_native_build_gets_the_current_admission_config_and_its_saved_path() {
        let dir = tempfile::tempdir().unwrap();
        let tool = |name: &str, body: &str| {
            let p = dir.path().join(name);
            burrow_test::executable(&p, format!("#!/bin/sh\n{body}\n"));
            p
        };
        let (_rig, mut cfg) = crate::usage::tests::rig();
        cfg.tools.systemctl = tool("systemctl", "echo inactive");
        cfg.tools.systemd_run = tool(
            "run",
            &format!("echo \"$@\" > {}/args", dir.path().display()),
        );
        let state = dir.path().join("builds");
        let proof = state.join("proof-o-r-1");
        fs::create_dir_all(&proof).unwrap();
        let inputs = r#"{"repo":"o/r","head":"original-head","drvs":{"check":"original.drv"}}"#;
        fs::write(proof.join("build.json"), inputs).unwrap();
        fs::write(proof.join("policy-paused"), "paused").unwrap();
        fs::write(
            proof.join("policy-env.json"),
            json!({"environment":"PATH=/tools AUTOLAND_CONFIG=/old/autoland.json"}).to_string(),
        )
        .unwrap();
        cfg.proofs_state = Some(state);
        cfg.proof_build_driver = vec!["/driver".into(), "steward.py".into()];
        cfg.proof_driver_config = Some("/new/autoland.json".into());
        cfg.settings = Some(burrow_settings::Config {
            overlay: dir.path().join("overlay.json"),
            allowed: vec!["o/r".into()],
            pipeline_repos: vec![],
        });
        reconcile(&cfg).unwrap();
        let args = fs::read_to_string(dir.path().join("args")).unwrap();
        assert!(
            args.contains("--setenv=AUTOLAND_CONFIG=/new/autoland.json"),
            "{args}"
        );
        assert!(args.contains("--setenv=PATH=/tools"), "{args}");
        assert!(!args.contains("/old/"), "{args}");
        assert_eq!(
            fs::read_to_string(proof.join("build.json")).unwrap(),
            inputs
        );
    }
}
