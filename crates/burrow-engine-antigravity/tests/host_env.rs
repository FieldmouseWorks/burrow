use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use burrow_engine_antigravity::config::{Config, CredentialMode, EFFORTS, ENGINE, ModelSpec};
use conary_core::plugin::{CONTRACT, Context, Execution, Identity, Negotiated, Request, Task};
use tempfile::TempDir;

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    burrow_test::executable(&path, body);
    path
}

fn sample_models() -> Vec<ModelSpec> {
    vec![ModelSpec {
        id: "test-model".to_owned(),
        efforts: EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
        context_tokens: Some(200_000),
        max_output_tokens: Some(16_384),
    }]
}

fn run_plugin_and_dump_env(
    dir: &Path,
    host_mode: bool,
    configure_env: impl FnOnce(&mut Command),
) -> String {
    let env_dump = dir.join("child-env.txt");
    let fixture = write_executable(
        dir,
        "ag-dump-env",
        &format!(
            "#!/bin/sh\ncat >/dev/null\nenv > \"{}\"\nprintf '{{\"type\":\"result\",\"conversation_id\":\"env-1\",\"result\":\"ok\"}}\\n'\n",
            env_dump.display(),
        ),
    );
    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_mode: CredentialMode::Env,
        credential_env: None,
        target_credential_env: None,
        host_mode,
        ..Config::default()
    };
    let cfg_path = dir.join("config.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let req = Request {
        contract: CONTRACT.to_owned(),
        execution: Some(Execution {
            choice: None,
            context: Context::Fresh,
        }),
        negotiated: Some(Negotiated {
            by: Identity {
                engine: ENGINE.to_owned(),
                session: None,
            },
            offers: cfg.offers(),
        }),
        task: Task::Job {
            repo: "owner/demo".to_owned(),
            workdir: dir.to_string_lossy().into_owned(),
            prompt: "check env".to_owned(),
            model: None,
            effort: None,
        },
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();

    let mut cmd = Command::new(burrow_test::program!("burrow-engine-antigravity"));
    cmd.arg(&cfg_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_env(&mut cmd);

    let mut child = cmd.spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(&req_bytes).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "plugin failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::read_to_string(&env_dump).unwrap()
}

fn env_value<'a>(dumped: &'a str, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}=");
    dumped
        .lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
}

#[test]
fn host_mode_forwards_real_home_gh_config_dir_and_xdg_git_config_when_unset() {
    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    let gh_dir = real_home.join(".config/gh");
    let git_cfg = real_home.join(".config/git/config");
    fs::create_dir_all(&gh_dir).unwrap();
    fs::write(gh_dir.join("hosts.yml"), "github.com:\n").unwrap();
    fs::create_dir_all(git_cfg.parent().unwrap()).unwrap();
    fs::write(&git_cfg, "[user]\n\tname = Test\n").unwrap();

    let dumped = run_plugin_and_dump_env(dir.path(), true, |cmd| {
        cmd.env("HOME", &real_home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GH_CONFIG_DIR")
            .env_remove("GH_TOKEN_SOCKET")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_ENTERPRISE_TOKEN")
            .env_remove("GITHUB_ENTERPRISE_TOKEN")
            .env_remove("GIT_CONFIG_GLOBAL");
    });

    let child_home = env_value(&dumped, "HOME").expect("HOME must be set");
    assert_ne!(Path::new(child_home), real_home);
    assert_eq!(
        env_value(&dumped, "GH_CONFIG_DIR"),
        Some(gh_dir.to_str().unwrap()),
        "host mode must forward GH_CONFIG_DIR when unset:\n{dumped}"
    );
    assert_eq!(
        env_value(&dumped, "GIT_CONFIG_GLOBAL"),
        Some(git_cfg.to_str().unwrap()),
        "host mode must fall back to XDG git config when ~/.gitconfig is absent:\n{dumped}"
    );
}

#[test]
fn host_mode_honors_xdg_config_home_for_gh_and_git_config() {
    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    let xdg_home = dir.path().join("custom-xdg");
    let gh_dir = xdg_home.join("gh");
    let git_cfg = xdg_home.join("git/config");
    fs::create_dir_all(&real_home).unwrap();
    fs::create_dir_all(&gh_dir).unwrap();
    fs::create_dir_all(git_cfg.parent().unwrap()).unwrap();
    fs::write(&git_cfg, "[user]\n\tname = XDG\n").unwrap();

    let dumped = run_plugin_and_dump_env(dir.path(), true, |cmd| {
        cmd.env("HOME", &real_home)
            .env("XDG_CONFIG_HOME", &xdg_home)
            .env_remove("GH_CONFIG_DIR")
            .env_remove("GH_TOKEN_SOCKET")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_ENTERPRISE_TOKEN")
            .env_remove("GITHUB_ENTERPRISE_TOKEN")
            .env_remove("GIT_CONFIG_GLOBAL");
    });

    assert_eq!(
        env_value(&dumped, "GH_CONFIG_DIR"),
        Some(gh_dir.to_str().unwrap()),
        "host mode must honor XDG_CONFIG_HOME for GH_CONFIG_DIR:\n{dumped}"
    );
    assert_eq!(
        env_value(&dumped, "GIT_CONFIG_GLOBAL"),
        Some(git_cfg.to_str().unwrap()),
        "host mode must honor XDG_CONFIG_HOME for GIT_CONFIG_GLOBAL:\n{dumped}"
    );
}

#[test]
fn host_mode_preserves_explicit_gh_config_dir_and_app_socket_isolation() {
    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    fs::create_dir_all(real_home.join(".config/gh")).unwrap();
    fs::write(real_home.join(".gitconfig"), "[user]\n\tname = Host\n").unwrap();

    // Case 1: explicit GH_CONFIG_DIR (as set by --as workers) and GIT_CONFIG_GLOBAL.
    let explicit_gh = "/run/gh-token/no-login";
    let explicit_git = dir.path().join("explicit-gitconfig");
    let dumped = run_plugin_and_dump_env(dir.path(), true, |cmd| {
        cmd.env("HOME", &real_home)
            .env("GH_CONFIG_DIR", explicit_gh)
            .env("GIT_CONFIG_GLOBAL", &explicit_git)
            .env_remove("GH_TOKEN_SOCKET");
    });
    assert_eq!(env_value(&dumped, "GH_CONFIG_DIR"), Some(explicit_gh));
    assert_eq!(
        env_value(&dumped, "GIT_CONFIG_GLOBAL"),
        Some(explicit_git.to_str().unwrap())
    );

    // Case 2: GH_TOKEN_SOCKET or GH_TOKEN set without GH_CONFIG_DIR must not fall back to host ~/.config/gh.
    let dumped_socket = run_plugin_and_dump_env(dir.path(), true, |cmd| {
        cmd.env("HOME", &real_home)
            .env("GH_TOKEN_SOCKET", "/run/gh-token/workers.sock")
            .env_remove("GH_CONFIG_DIR")
            .env_remove("GH_TOKEN")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GIT_CONFIG_GLOBAL");
    });
    assert_eq!(
        env_value(&dumped_socket, "GH_CONFIG_DIR"),
        None,
        "GH_TOKEN_SOCKET jobs must not inherit real HOME's gh config:\n{dumped_socket}"
    );

    let dumped_token = run_plugin_and_dump_env(dir.path(), true, |cmd| {
        cmd.env("HOME", &real_home)
            .env("GH_TOKEN", "ghs_scoped_token")
            .env_remove("GH_CONFIG_DIR")
            .env_remove("GH_TOKEN_SOCKET")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_ENTERPRISE_TOKEN")
            .env_remove("GITHUB_ENTERPRISE_TOKEN");
    });
    assert_eq!(
        env_value(&dumped_token, "GH_CONFIG_DIR"),
        None,
        "GH_TOKEN jobs must not inherit real HOME's gh config:\n{dumped_token}"
    );

    for ent_key in ["GH_ENTERPRISE_TOKEN", "GITHUB_ENTERPRISE_TOKEN"] {
        let dumped_ent = run_plugin_and_dump_env(dir.path(), true, |cmd| {
            cmd.env("HOME", &real_home)
                .env(ent_key, "ghs_enterprise_token")
                .env_remove("GH_CONFIG_DIR")
                .env_remove("GH_TOKEN_SOCKET")
                .env_remove("GH_TOKEN")
                .env_remove("GITHUB_TOKEN")
                .env_remove(if ent_key == "GH_ENTERPRISE_TOKEN" {
                    "GITHUB_ENTERPRISE_TOKEN"
                } else {
                    "GH_ENTERPRISE_TOKEN"
                });
        });
        assert_eq!(
            env_value(&dumped_ent, "GH_CONFIG_DIR"),
            None,
            "{ent_key} jobs must not inherit real HOME's gh config:\n{dumped_ent}"
        );
    }
}

#[test]
fn non_host_mode_does_not_forward_gh_config_dir() {
    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    fs::create_dir_all(real_home.join(".config/gh")).unwrap();

    let dumped = run_plugin_and_dump_env(dir.path(), false, |cmd| {
        cmd.env("HOME", &real_home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("GH_CONFIG_DIR")
            .env_remove("GH_TOKEN_SOCKET");
    });

    assert_eq!(env_value(&dumped, "GH_CONFIG_DIR"), None);
}
