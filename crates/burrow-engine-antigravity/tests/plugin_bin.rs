use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use burrow_engine_antigravity::STRIPPED_ENV;
use burrow_engine_antigravity::config::{
    CONFIG_ENV, Config, CredentialMode, DEFAULT_CREDENTIAL_HOME_PATH, EFFORTS, ENGINE, Effort,
    ModelSpec, OutputFormat,
};
use conary_core::plugin::{
    Answer, CONTRACT, Choice, Context, Execution, Identity, Kind, Limits, Negotiated, Next, Plugin,
    Request, Task,
};
use tempfile::TempDir;

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    burrow_test::executable(&path, body);
    path
}

fn sample_models() -> Vec<ModelSpec> {
    vec![
        ModelSpec {
            id: "test-model".to_owned(),
            efforts: EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
            context_tokens: Some(200_000),
            max_output_tokens: Some(16_384),
        },
        ModelSpec {
            id: "alt-model".to_owned(),
            efforts: vec!["low".to_owned(), "high".to_owned(), "max".to_owned()],
            context_tokens: None,
            max_output_tokens: None,
        },
    ]
}

#[test]
fn github_tokens_are_stripped_and_isolated_file_credentials_are_cleaned_up() {
    let dir = TempDir::new().unwrap();
    let env_dump = dir.path().join("child-env.txt");
    let cred_path_dump = dir.path().join("cred-path.txt");

    let fixture = write_executable(
        dir.path(),
        "ag-env-check",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
env > "{}"
if [ -n "$AG_CRED_FILE" ] && [ -r "$AG_CRED_FILE" ]; then
  printf '%s\n' "$AG_CRED_FILE" > "{}"
  val=$(cat "$AG_CRED_FILE")
  printf '{{"type":"result","conversation_id":"env-1","result":"cred=%s"}}\n' "$val"
else
  printf '{{"type":"result","conversation_id":"env-1","result":"no-cred"}}\n'
fi
"#,
            env_dump.display(),
            cred_path_dump.display(),
        ),
    );

    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_env: Some("BURROW_AG_TEST_IN".to_owned()),
        target_credential_env: Some("AG_CRED_FILE".to_owned()),
        credential_mode: CredentialMode::File,
        ..Config::default()
    };
    let cfg_path = dir.path().join("config.json");
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
            workdir: dir.path().to_string_lossy().into_owned(),
            prompt: "check env".to_owned(),
            model: None,
            effort: None,
        },
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();

    let mut child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&cfg_path)
        .env("BURROW_AG_TEST_IN", "isolated-antigravity-secret")
        .env("GH_TOKEN", "ghp_must_never_leak")
        .env("GITHUB_TOKEN", "ghp_must_never_leak_2")
        .env("GH_TOKEN_SOCKET", "/run/gh-token/workers.sock")
        .env("CLAUDE_CODE_OAUTH_TOKEN", "claude_must_never_leak")
        .env("ANTHROPIC_API_KEY", "sk-ant-must-never-leak")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(&req_bytes).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dumped_env = fs::read_to_string(&env_dump).unwrap();
    for forbidden in STRIPPED_ENV {
        assert!(
            !dumped_env
                .lines()
                .any(|l| l.starts_with(&format!("{forbidden}="))),
            "child env leaked {forbidden}:\n{dumped_env}"
        );
    }
    assert!(
        !dumped_env
            .lines()
            .any(|l| l.starts_with("BURROW_AG_TEST_IN=")),
        "source credential env should not leak when target_credential_env differs"
    );

    let cred_path = PathBuf::from(fs::read_to_string(&cred_path_dump).unwrap().trim());
    assert!(
        !cred_path.exists(),
        "temp credential file should be cleaned up after exit"
    );
}

#[test]
fn conary_runner_v5_end_to_end_check_fresh_and_continue() {
    let dir = TempDir::new().unwrap();
    let fixture = write_executable(
        dir.path(),
        "fake-ag",
        r#"#!/bin/sh
cat >/dev/null
conv=""
model=""
effort=""
prev=""
for a in "$@"; do
  case "$prev" in
    --conversation) conv="$a" ;;
    --model) model="$a" ;;
    --effort) effort="$a" ;;
  esac
  prev="$a"
done
if [ -n "$conv" ]; then
  printf '{"type":"result","conversation_id":"%s","result":"cont:%s:%s"}\n' "$conv" "$model" "$effort"
else
  printf '{"type":"result","conversation_id":"ag-sess-9","result":"fresh:%s:%s"}\n' "$model" "$effort"
fi
"#,
    );

    let cfg = Config {
        binary: fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        default_effort: Some(Effort::High),
        meters: vec!["quota".to_owned()],
        ..Config::default()
    };
    let cfg_path = dir.path().join("plugin.json");
    fs::write(&cfg_path, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let wrapper = write_executable(
        dir.path(),
        "antigravity-plugin",
        &format!(
            "#!/bin/sh\nexport {CONFIG_ENV}=\"{}\"\nexec \"{}\"\n",
            cfg_path.display(),
            burrow_test::program!("burrow-engine-antigravity"),
        ),
    );
    let limits = Limits {
        deadline: Duration::from_secs(15),
        response_bytes: 1_048_576,
    };

    // 1. Conary's own conformance check passes.
    let plugin = Plugin::describe(&wrapper, limits).unwrap();
    assert_eq!(plugin.identity.engine, ENGINE);
    let checked = plugin.check().unwrap();
    assert_eq!(checked, vec![Kind::Job]);

    // 2. Fresh v5 job through Conary's real runner.
    let fresh_task = Task::Job {
        repo: "owner/demo".to_owned(),
        workdir: dir.path().to_string_lossy().into_owned(),
        prompt: "first turn".to_owned(),
        model: Some("test-model".to_owned()),
        effort: Some("xhigh".to_owned()),
    };
    let fresh = plugin
        .call_with(
            fresh_task,
            Some(Choice {
                engine: ENGINE.to_owned(),
                model: Some("test-model".to_owned()),
                effort: Some("xhigh".to_owned()),
            }),
            Context::Fresh,
            None,
        )
        .unwrap();
    assert_eq!(fresh.by.session.as_deref(), Some("ag-sess-9"));
    assert!(matches!(
        fresh.answer,
        Answer::Job { ref summary, next: Next::Done } if summary == "fresh:test-model:xhigh"
    ));

    // 3. Continued v5 job through Conary's real runner.
    let cont_task = Task::Job {
        repo: "owner/demo".to_owned(),
        workdir: dir.path().to_string_lossy().into_owned(),
        prompt: "second turn".to_owned(),
        model: Some("alt-model".to_owned()),
        effort: Some("max".to_owned()),
    };
    let cont = plugin
        .call_with(
            cont_task,
            Some(Choice {
                engine: ENGINE.to_owned(),
                model: Some("alt-model".to_owned()),
                effort: Some("max".to_owned()),
            }),
            Context::Continue {
                session: "ag-sess-9".to_owned(),
            },
            None,
        )
        .unwrap();
    assert_eq!(cont.by.session.as_deref(), Some("ag-sess-9"));
    assert!(matches!(
        cont.answer,
        Answer::Job { ref summary, next: Next::Done } if summary == "cont:alt-model:max"
    ));
}

#[test]
fn home_file_mode_materializes_oauth_token_strips_envs_and_classifies_login_refusal() {
    let dir = TempDir::new().unwrap();
    let fake_home = TempDir::new().unwrap();
    let env_dump = dir.path().join("home-file-env.txt");
    let token_payload = r#"{"access_token":"ya29.bin-oauth-secret-access","refresh_token":"1//bin-oauth-secret-refresh"}"#;

    let ok_fixture = write_executable(
        dir.path(),
        "ag-home-file-bin",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
env > "{}"
tok="$HOME/{DEFAULT_CREDENTIAL_HOME_PATH}"
if [ -f "$tok" ] && [ -w "$tok" ]; then
  val=$(cat "$tok")
  printf '{{"type":"result","conversation_id":"hf-bin-1","result":"loaded:%s"}}\n' "${{#val}}"
else
  printf '{{"status":"ERROR","error":"missing token"}}\n'
  exit 1
fi
"#,
            env_dump.display()
        ),
    );

    let cfg = Config {
        binary: ok_fixture,
        models: sample_models(),
        default_model: Some("test-model".to_owned()),
        credential_env: Some("BURROW_ANTIGRAVITY_CREDENTIAL".to_owned()),
        target_credential_env: Some("ANTIGRAVITY_API_KEY".to_owned()),
        credential_mode: CredentialMode::HomeFile,
        ..Config::default()
    };
    let cfg_path = dir.path().join("home-file-config.json");
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
            workdir: dir.path().to_string_lossy().into_owned(),
            prompt: "check home-file".to_owned(),
            model: None,
            effort: None,
        },
    };
    let req_bytes = serde_json::to_vec(&req).unwrap();

    let mut child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&cfg_path)
        .env("HOME", fake_home.path())
        .env("BURROW_ANTIGRAVITY_CREDENTIAL", token_payload)
        .env("ANTIGRAVITY_API_KEY", "must-be-stripped-target-env")
        .env("GH_TOKEN", "ghp_must_never_leak")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.as_mut().unwrap().write_all(&req_bytes).unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let dumped_env = fs::read_to_string(&env_dump).unwrap();
    for forbidden in [
        "BURROW_ANTIGRAVITY_CREDENTIAL",
        "ANTIGRAVITY_API_KEY",
        "GH_TOKEN",
    ] {
        assert!(
            !dumped_env
                .lines()
                .any(|l| l.starts_with(&format!("{forbidden}="))),
            "child env leaked {forbidden}:\n{dumped_env}"
        );
    }

    let gemini_dir = fake_home.path().join(".gemini");
    let cli_dir = gemini_dir.join("antigravity-cli");
    let tok_path = cli_dir.join("antigravity-oauth-token");
    assert_eq!(
        fs::metadata(&gemini_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&cli_dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&tok_path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read_to_string(&tok_path).unwrap(), token_payload);

    // Unauthenticated CLI output classifies as login refusal and scrubs the OAuth secret.
    let unauth_fixture = write_executable(
        dir.path(),
        "ag-unauth-bin",
        r#"#!/bin/sh
echo "Authentication required. Please visit the URL to log in: https://example.test/auth?t=ya29.bin-oauth-secret-access" >&2
printf '{"status":"ERROR","error":"authentication failed or timed out"}\n'
exit 1
"#,
    );
    let mut unauth_cfg = cfg;
    unauth_cfg.binary = unauth_fixture;
    unauth_cfg.output_format = OutputFormat::Json;
    let unauth_cfg_path = dir.path().join("unauth-config.json");
    fs::write(&unauth_cfg_path, serde_json::to_vec(&unauth_cfg).unwrap()).unwrap();

    let mut unauth_child = Command::new(burrow_test::program!("burrow-engine-antigravity"))
        .arg(&unauth_cfg_path)
        .env("HOME", fake_home.path())
        .env("BURROW_ANTIGRAVITY_CREDENTIAL", token_payload)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    unauth_child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&req_bytes)
        .unwrap();
    drop(unauth_child.stdin.take());
    let unauth_output = unauth_child.wait_with_output().unwrap();
    assert!(!unauth_output.status.success());
    let stderr = String::from_utf8_lossy(&unauth_output.stderr);
    assert!(stderr.contains("antigravity login refusal"), "{stderr}");
    assert!(!stderr.contains("ya29.bin-oauth-secret-access"), "{stderr}");
    assert!(stderr.contains("[redacted]"), "{stderr}");
}
