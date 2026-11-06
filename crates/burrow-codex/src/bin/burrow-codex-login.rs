//! One login tool, with a separate refresh-token family for each job user.
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    env, fs,
    os::unix::process::CommandExt,
    path::PathBuf,
    process::{Command, ExitCode},
};

#[derive(Deserialize)]
struct Target {
    user: String,
    home: PathBuf,
}
#[derive(Deserialize)]
struct Config {
    sudo: PathBuf,
    env: PathBuf,
    codex: PathBuf,
    targets: BTreeMap<String, Target>,
}

fn command(cfg: &Config, target: &str, extra: &[String]) -> Result<Command, String> {
    let t = cfg.targets.get(target).ok_or_else(|| {
        format!(
            "no {target} login here; choose {}",
            cfg.targets.keys().cloned().collect::<Vec<_>>().join(" or ")
        )
    })?;
    let mut c = Command::new(&cfg.sudo);
    c.args(["-u", &t.user, "--"])
        .arg(&cfg.env)
        .arg(format!("HOME={}", t.home.display()))
        .arg(format!("CODEX_HOME={}/.codex", t.home.display()))
        .arg(&cfg.codex)
        .args(["login", "--device-auth"])
        .args(extra);
    Ok(c)
}
fn run() -> Result<(), String> {
    let mut args = env::args().skip(1);
    let config = args.next().ok_or("login config missing")?;
    let cfg: Config = serde_json::from_slice(&fs::read(config).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let target = args.next().unwrap_or_else(|| "review".into());
    let mut command = command(&cfg, &target, &args.collect::<Vec<_>>())?;
    Err(command.exec().to_string())
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("autoland-codex-login: {e}");
            ExitCode::FAILURE
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_user_has_its_own_home_and_no_unknown_user_is_run() {
        let cfg: Config = serde_json::from_value(serde_json::json!({"sudo":"/sudo", "env":"/env", "codex":"/codex", "targets":{
            "review":{"user":"reviewer", "home":"/reviewer"}, "job":{"user":"worker", "home":"/worker"}}})).unwrap();
        for (key, user) in [("review", "reviewer"), ("job", "worker")] {
            let c = command(&cfg, key, &[]).unwrap();
            let args: Vec<_> = c.get_args().map(|s| s.to_str().unwrap()).collect();
            assert_eq!(
                args,
                [
                    "-u",
                    user,
                    "--",
                    "/env",
                    &format!("HOME=/{user}"),
                    &format!("CODEX_HOME=/{user}/.codex"),
                    "/codex",
                    "login",
                    "--device-auth"
                ]
            );
        }
        assert!(command(&cfg, "stranger", &[]).is_err());
    }
}
