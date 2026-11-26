use std::fs;
use std::io::Write;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use burrow_engine_antigravity::config::{
    Config, CredentialMode, DEFAULT_CREDENTIAL_HOME_PATH, EFFORTS, ENGINE, ModelSpec,
};
use conary_core::plugin::{
    Answer, CONTRACT, Context, Execution, Identity, Negotiated, Request, Response, Task,
};
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

fn run_plugin(
    cfg_path: &Path,
    cfg: &Config,
    workdir: &Path,
    context: Context,
    configure_env: impl FnOnce(&mut Command),
) -> Output {
    let req = Request {
        contract: CONTRACT.to_owned(),
        execution: Some(Execution {
            choice: None,
            context,
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
            workdir: workdir.to_string_lossy().into_owned(),
            prompt: "continue work".to_owned(),
            model: None,
            effort: None,
        },
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();

    let mut cmd = Command::new(burrow_test::program!("burrow-engine-antigravity"));
    cmd.arg(cfg_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_env(&mut cmd);

    let mut child = cmd.spawn().unwrap();
    child.stdin.as_mut().unwrap().write_all(&req_bytes).unwrap();
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}

#[test]
fn host_mode_persists_home_across_continuation_turns_when_proj_engine_home_is_set() {
    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    fs::create_dir_all(&real_home).unwrap();
    let engine_home = dir.path().join("job-dir").join("engine-home");
    fs::create_dir_all(engine_home.parent().unwrap()).unwrap();
    let cred_src = dir.path().join("host-oauth.json");
    fs::write(&cred_src, r#"{"access_token":"secret-host-token"}"#).unwrap();
    let env_dump = dir.path().join("child-env.txt");

    // Mirrors real `antigravity` CLI behavior: conversations live under
    // `$HOME/.gemini/antigravity-cli/conversations/<id>`. Continuing an ID that
    // is missing from `$HOME` starts a new session with a fresh ID.
    let fixture = write_executable(
        dir.path(),
        "ag-session-store",
        &format!(
            r#"#!/bin/sh
set -eu
cat >/dev/null
env > "{env_dump}"
conv=""
while [ $# -gt 0 ]; do
  if [ "$1" = "--conversation" ]; then
    conv="$2"
    shift 2
    continue
  fi
  shift
done
tok="$HOME/{cred_rel}"
if [ ! -f "$tok" ]; then
  printf '{{"status":"ERROR","error":"missing oauth token"}}\n'
  exit 1
fi
store="$HOME/.gemini/antigravity-cli/conversations"
mkdir -p "$store"
if [ -z "$conv" ]; then
  sid="sess-turn-1"
  printf 'turn1\n' > "$store/$sid"
  printf '{{"type":"result","conversation_id":"%s","result":"turn1-ok"}}\n' "$sid"
elif [ -f "$store/$conv" ]; then
  printf 'turn2\n' >> "$store/$conv"
  printf '{{"type":"result","conversation_id":"%s","result":"resumed-%s"}}\n' "$conv" "$conv"
else
  sid="sess-new-because-home-was-wiped"
  printf 'fresh\n' > "$store/$sid"
  printf '{{"type":"result","conversation_id":"%s","result":"started-fresh"}}\n' "$sid"
fi
"#,
            env_dump = env_dump.display(),
            cred_rel = DEFAULT_CREDENTIAL_HOME_PATH,
        ),
    );

    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_mode: CredentialMode::HomeFile,
        credential_file: Some(cred_src),
        host_mode: true,
        ..Config::default()
    };
    let cfg_path = dir.path().join("config.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    // Turn 1: Fresh context.
    let out1 = run_plugin(&cfg_path, &cfg, dir.path(), Context::Fresh, |cmd| {
        cmd.env("HOME", &real_home)
            .env("PROJ_ENGINE_HOME", &engine_home);
    });
    assert!(
        out1.status.success(),
        "turn 1 failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out1.stdout),
        String::from_utf8_lossy(&out1.stderr)
    );
    let resp1: Response = serde_json::from_slice(&out1.stdout).unwrap();
    assert_eq!(resp1.by.session.as_deref(), Some("sess-turn-1"));

    // Persistent host HOME exists with 0700, kept the conversation, did not touch real HOME,
    // stripped PROJ_ENGINE_HOME from child env, and removed the materialized token on turn exit.
    assert_eq!(
        fs::metadata(&engine_home).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert!(
        engine_home
            .join(".gemini/antigravity-cli/conversations/sess-turn-1")
            .is_file()
    );
    assert!(
        !engine_home.join(DEFAULT_CREDENTIAL_HOME_PATH).exists(),
        "materialized home-file credential must be cleaned up between turns"
    );
    assert!(!real_home.join(".gemini").exists());
    let dumped = fs::read_to_string(&env_dump).unwrap();
    assert!(
        !dumped
            .lines()
            .any(|line| line.starts_with("PROJ_ENGINE_HOME=")),
        "PROJ_ENGINE_HOME must be stripped from child antigravity env:\n{dumped}"
    );

    // Turn 2: Continue the same session.
    let out2 = run_plugin(
        &cfg_path,
        &cfg,
        dir.path(),
        Context::Continue {
            session: "sess-turn-1".to_owned(),
        },
        |cmd| {
            cmd.env("HOME", &real_home)
                .env("PROJ_ENGINE_HOME", &engine_home);
        },
    );
    assert!(
        out2.status.success(),
        "turn 2 continuation failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out2.stdout),
        String::from_utf8_lossy(&out2.stderr)
    );
    let resp2: Response = serde_json::from_slice(&out2.stdout).unwrap();
    assert_eq!(resp2.by.session.as_deref(), Some("sess-turn-1"));
    assert!(
        matches!(resp2.answer, Answer::Job { ref summary, .. } if summary == "resumed-sess-turn-1"),
        "{:?}",
        resp2.answer
    );
    assert!(
        !engine_home.join(DEFAULT_CREDENTIAL_HOME_PATH).exists(),
        "materialized home-file credential must be cleaned up after turn 2"
    );
}

#[test]
fn host_mode_rejects_unsafe_proj_engine_home_values() {
    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    fs::create_dir_all(&real_home).unwrap();
    let fixture = write_executable(
        dir.path(),
        "ag-noop",
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"type\":\"result\",\"conversation_id\":\"s1\",\"result\":\"ok\"}\\n'\n",
    );
    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_mode: CredentialMode::Env,
        host_mode: true,
        ..Config::default()
    };
    let cfg_path = dir.path().join("config.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    // Case 1: relative path.
    let rel = run_plugin(&cfg_path, &cfg, dir.path(), Context::Fresh, |cmd| {
        cmd.env("HOME", &real_home)
            .env("PROJ_ENGINE_HOME", "relative/engine-home");
    });
    assert!(!rel.status.success());
    assert!(
        String::from_utf8_lossy(&rel.stderr).contains("absolute"),
        "{}",
        String::from_utf8_lossy(&rel.stderr)
    );

    // Case 2: symlink target.
    let outside = dir.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let sym = dir.path().join("sym-home");
    symlink(&outside, &sym).unwrap();
    let sym_out = run_plugin(&cfg_path, &cfg, dir.path(), Context::Fresh, |cmd| {
        cmd.env("HOME", &real_home).env("PROJ_ENGINE_HOME", &sym);
    });
    assert!(!sym_out.status.success());
    assert!(
        String::from_utf8_lossy(&sym_out.stderr).contains("symlink"),
        "{}",
        String::from_utf8_lossy(&sym_out.stderr)
    );

    // Case 3: regular file.
    let reg = dir.path().join("not-a-dir");
    fs::write(&reg, "file").unwrap();
    let reg_out = run_plugin(&cfg_path, &cfg, dir.path(), Context::Fresh, |cmd| {
        cmd.env("HOME", &real_home).env("PROJ_ENGINE_HOME", &reg);
    });
    assert!(!reg_out.status.success());
    assert!(
        String::from_utf8_lossy(&reg_out.stderr).contains("not a directory"),
        "{}",
        String::from_utf8_lossy(&reg_out.stderr)
    );

    // Case 4: equal to real HOME.
    let same_out = run_plugin(&cfg_path, &cfg, dir.path(), Context::Fresh, |cmd| {
        cmd.env("HOME", &real_home)
            .env("PROJ_ENGINE_HOME", &real_home);
    });
    assert!(!same_out.status.success());
    assert!(
        String::from_utf8_lossy(&same_out.stderr).contains("HOME"),
        "{}",
        String::from_utf8_lossy(&same_out.stderr)
    );
}

#[test]
fn host_mode_falls_back_to_ephemeral_tempdir_when_proj_engine_home_unset() {
    let dir = TempDir::new().unwrap();
    let real_home = dir.path().join("real-home");
    fs::create_dir_all(&real_home).unwrap();
    let seen_home_file = dir.path().join("seen-home");
    let fixture = write_executable(
        dir.path(),
        "ag-print-home",
        &format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' \"$HOME\" > \"{}\"\nprintf '{{\"type\":\"result\",\"conversation_id\":\"s1\",\"result\":\"ok\"}}\\n'\n",
            seen_home_file.display()
        ),
    );
    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_mode: CredentialMode::Env,
        host_mode: true,
        ..Config::default()
    };
    let cfg_path = dir.path().join("config.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let out = run_plugin(&cfg_path, &cfg, dir.path(), Context::Fresh, |cmd| {
        cmd.env("HOME", &real_home).env_remove("PROJ_ENGINE_HOME");
    });
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let seen_home = PathBuf::from(fs::read_to_string(&seen_home_file).unwrap());
    assert_ne!(seen_home, real_home);
    assert!(
        seen_home
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("antigravity-host-home-")),
        "{}",
        seen_home.display()
    );
    assert!(!seen_home.exists());
}
