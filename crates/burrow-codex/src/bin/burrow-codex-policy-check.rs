//! Check the generated R2 policy with the same CLI the jobs run.
use serde::Deserialize;
use std::{
    env, fs,
    path::Path,
    process::{Command, ExitCode},
};
#[derive(Deserialize)]
struct Rule {
    pattern: String,
    prefix: Vec<String>,
}
#[derive(Deserialize)]
struct Expected {
    patterns: Vec<String>,
    commands: Vec<Rule>,
}
fn check(codex: &Path, expected: &Path, rules: &Path) -> Result<(), String> {
    let expected: Expected =
        serde_json::from_slice(&fs::read(expected).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    for pattern in &expected.patterns {
        if !expected.commands.iter().any(|r| &r.pattern == pattern) {
            return Err(format!("no rule generated for R2 pattern: {pattern}"));
        }
    }
    for rule in expected.commands {
        for sudo in [false, true] {
            let mut c = Command::new(codex);
            c.args(["execpolicy", "check", "--rules"]).arg(rules);
            if sudo {
                c.arg("sudo");
            }
            let out = c.args(&rule.prefix).output().map_err(|e| e.to_string())?;
            let v: serde_json::Value =
                serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
            if !out.status.success() || v["decision"] != "forbidden" {
                return Err(format!(
                    "no forbidden rule for {} (sudo={sudo})",
                    rule.pattern
                ));
            }
        }
    }
    Ok(())
}
fn main() -> ExitCode {
    let args: Vec<_> = env::args_os().collect();
    let result = if args.len() == 4 {
        check(
            Path::new(&args[1]),
            Path::new(&args[2]),
            Path::new(&args[3]),
        )
    } else {
        Err("usage: policy-check CODEX EXPECTED_JSON RULES".into())
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
