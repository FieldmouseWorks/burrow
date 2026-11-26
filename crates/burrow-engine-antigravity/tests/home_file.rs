use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use burrow_engine_antigravity::{
    Config, CredentialMode, DEFAULT_CREDENTIAL_HOME_PATH, EFFORTS, Effort, Invocation, ModelSpec,
    OutputFormat, Refusal, RunError, normalize_output, run_invocation,
    validate_credential_home_path,
};
use tempfile::TempDir;

fn write_executable(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    burrow_test::executable(&path, body);
    path
}

fn base_config(binary: PathBuf) -> Config {
    Config {
        binary,
        models: vec![ModelSpec {
            id: "test-model".to_owned(),
            efforts: EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
            context_tokens: Some(200_000),
            max_output_tokens: Some(16_384),
        }],
        default_model: Some("test-model".to_owned()),
        default_effort: Some(Effort::Medium),
        deadline_secs: 30,
        meters: vec!["quota".to_owned()],
        ..Config::default()
    }
}

#[test]
fn credential_home_path_serde_and_validation_enforce_relative_normal_paths() {
    let parsed: Config = serde_json::from_str(r#"{"credential_mode":"home-file"}"#).unwrap();
    assert_eq!(parsed.credential_mode, CredentialMode::HomeFile);
    assert_eq!(
        parsed.credential_home_path,
        PathBuf::from(DEFAULT_CREDENTIAL_HOME_PATH)
    );
    assert!(parsed.validate().is_ok());

    for valid in [
        DEFAULT_CREDENTIAL_HOME_PATH,
        "oauth-token",
        ".config/antigravity/token.json",
    ] {
        assert!(
            validate_credential_home_path(Path::new(valid)).is_ok(),
            "{valid}"
        );
    }

    for invalid in [
        "",
        "/",
        "/etc/passwd",
        ".",
        "..",
        "./token",
        "../token",
        ".gemini/./token",
        ".gemini/../token",
        ".gemini/token/.",
        ".gemini/token/",
        ".gemini//token",
    ] {
        assert!(
            validate_credential_home_path(Path::new(invalid)).is_err(),
            "expected {invalid:?} to be rejected"
        );
        let cfg = Config {
            credential_mode: CredentialMode::HomeFile,
            credential_home_path: PathBuf::from(invalid),
            ..Config::default()
        };
        assert!(
            cfg.validate().is_err(),
            "Config::validate accepted {invalid:?}"
        );
    }
}

#[test]
fn home_file_materializes_private_writable_oauth_token_and_handles_edge_cases() {
    let dir = TempDir::new().unwrap();
    let home = TempDir::new().unwrap();
    let src_cred = dir.path().join("oauth.json");
    let token_json = r#"{"access_token":"ya29.oauth-access-secret-1234","refresh_token":"1//oauth-refresh-secret-5678"}"#;
    fs::write(&src_cred, format!("{token_json}\n")).unwrap();

    let fixture = write_executable(
        dir.path(),
        "ag-home-file",
        &format!(
            r#"#!/bin/sh
cat >/dev/null
tok="$HOME/{DEFAULT_CREDENTIAL_HOME_PATH}"
if [ ! -f "$tok" ] || [ ! -w "$tok" ]; then
  printf '{{"status":"ERROR","error":"missing or unwritable token file"}}\n'
  exit 1
fi
val=$(cat "$tok")
printf '{{"access_token":"refreshed-token"}}' > "$tok"
printf '{{"type":"result","conversation_id":"hf-1","result":"ok:%s"}}\n' "${{#val}}"
"#
        ),
    );

    let mut cfg = base_config(fixture);
    cfg.credential_mode = CredentialMode::HomeFile;
    cfg.credential_file = Some(src_cred.clone());

    let mut inv = Invocation::from_config(
        &cfg,
        dir.path().to_path_buf(),
        "turn 1".to_owned(),
        Some("test-model".to_owned()),
        Some(Effort::Low),
        None,
    );
    inv.home_dir = Some(home.path().to_path_buf());

    let turn = run_invocation(&cfg, &inv, &|| false);
    assert_eq!(turn.error, None, "{turn:?}");
    assert_eq!(
        turn.result.as_deref(),
        Some(format!("ok:{}", token_json.len()).as_str())
    );

    let gemini_dir = home.path().join(".gemini");
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
    // The child refreshed the writable 0600 file in place; a subsequent turn overwrites it cleanly.
    assert_eq!(
        fs::read_to_string(&tok_path).unwrap(),
        r#"{"access_token":"refreshed-token"}"#
    );
    burrow_test::permissions(&tok_path, 0o400);
    let turn2 = run_invocation(&cfg, &inv, &|| false);
    assert_eq!(turn2.error, None, "{turn2:?}");
    assert_eq!(
        fs::metadata(&tok_path).unwrap().permissions().mode() & 0o777,
        0o600
    );

    // JSON OAuth tokens leaked in error output are scrubbed.
    let leak_fixture = write_executable(
        dir.path(),
        "ag-leak",
        r#"#!/bin/sh
cat >/dev/null
echo "Authentication required for ya29.oauth-access-secret-1234 and 1//oauth-refresh-secret-5678" >&2
exit 1
"#,
    );
    cfg.binary = leak_fixture;
    let leaked = run_invocation(&cfg, &inv, &|| false);
    match leaked.error {
        Some(RunError::Refusal(Refusal::Login { detail })) => {
            assert!(
                !detail.contains("ya29.oauth-access-secret-1234"),
                "{detail}"
            );
            assert!(!detail.contains("1//oauth-refresh-secret-5678"), "{detail}");
            assert!(detail.contains("[redacted]"), "{detail}");
        }
        other => panic!("expected scrubbed Login refusal, got {other:?}"),
    }

    // Symlink target inside $HOME is refused.
    let sym_home = TempDir::new().unwrap();
    let outside = dir.path().join("outside-target");
    fs::write(&outside, "untouched").unwrap();
    symlink(dir.path(), sym_home.path().join(".gemini")).unwrap();
    inv.home_dir = Some(sym_home.path().to_path_buf());
    let sym_turn = run_invocation(&cfg, &inv, &|| false);
    assert!(
        matches!(sym_turn.error, Some(RunError::ProcessFailed { ref message, .. }) if message.contains("symlink")),
        "{sym_turn:?}"
    );
    assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");

    // Empty/unset credential in home-file mode runs cleanly without panicking or creating a file.
    let empty_home = TempDir::new().unwrap();
    let empty_fixture = write_executable(
        dir.path(),
        "ag-empty-cred",
        "#!/bin/sh\ncat >/dev/null\nprintf '{\"type\":\"result\",\"result\":\"ran\"}\\n'\n",
    );
    let mut empty_cfg = base_config(empty_fixture);
    empty_cfg.credential_mode = CredentialMode::HomeFile;
    empty_cfg.credential_file = None;
    empty_cfg.credential_env = None;
    inv.home_dir = Some(empty_home.path().to_path_buf());
    let empty_turn = run_invocation(&empty_cfg, &inv, &|| false);
    assert_eq!(empty_turn.error, None, "{empty_turn:?}");
    assert!(
        !empty_home
            .path()
            .join(DEFAULT_CREDENTIAL_HOME_PATH)
            .exists()
    );
}

#[test]
fn unauthenticated_cli_outputs_classify_as_login_refusal_and_stdin_does_not_block() {
    // Real headless CLI stdout + stderr.
    let real_json = normalize_output(
        OutputFormat::Json,
        br#"{"status":"ERROR","error":"authentication failed or timed out","duration_ms":12}"#,
        b"Authentication required. Please visit the URL to log in:\nhttps://example.test/auth\n",
        Some(1),
        None,
        None,
        &[],
    );
    assert!(
        matches!(
            real_json.error,
            Some(RunError::Refusal(Refusal::Login { .. }))
        ),
        "{real_json:?}"
    );

    // Each diagnostic phrase on its own classifies as Refusal::Login.
    for (format, stdout, stderr) in [
        (
            OutputFormat::Json,
            br#"{"status":"ERROR","error":"authentication failed or timed out"}"#.as_slice(),
            b"".as_slice(),
        ),
        (
            OutputFormat::Json,
            b"".as_slice(),
            b"Authentication required\n".as_slice(),
        ),
        (
            OutputFormat::StreamJson,
            b"".as_slice(),
            b"Please visit the URL to log in: https://example.test/auth\n".as_slice(),
        ),
        (
            OutputFormat::Text,
            b"".as_slice(),
            b"Authentication required. Please visit the URL to log in:\n".as_slice(),
        ),
    ] {
        let turn = normalize_output(format, stdout, stderr, Some(1), None, None, &[]);
        assert!(
            matches!(turn.error, Some(RunError::Refusal(Refusal::Login { .. }))),
            "{format:?}: {turn:?}"
        );
    }

    // Fast-exiting unauthenticated CLI that does NOT read stdin (even with a
    // >128 KiB prompt) still returns Refusal::Login rather than EPIPE, and an
    // unauthenticated CLI reading stdin hits EOF immediately without hanging.
    let dir = TempDir::new().unwrap();
    let fast_exit = write_executable(
        dir.path(),
        "ag-unauth-fast",
        r#"#!/bin/sh
echo "Authentication required. Please visit the URL to log in:" >&2
printf '{"status":"ERROR","error":"authentication failed or timed out"}\n'
exit 1
"#,
    );
    let mut cfg = base_config(fast_exit);
    cfg.output_format = OutputFormat::StreamJson;
    let big_prompt = "x".repeat(256 * 1024);
    let inv = Invocation::from_config(
        &cfg,
        dir.path().to_path_buf(),
        big_prompt,
        Some("test-model".to_owned()),
        Some(Effort::Low),
        None,
    );
    let fast_turn = run_invocation(&cfg, &inv, &|| false);
    assert!(
        matches!(
            fast_turn.error,
            Some(RunError::Refusal(Refusal::Login { .. }))
        ),
        "{fast_turn:?}"
    );

    let read_stdin_unauth = write_executable(
        dir.path(),
        "ag-unauth-read",
        r#"#!/bin/sh
echo "Authentication required. Please visit the URL to log in:" >&2
# Empty prompt gives /dev/null; reading stdin must hit EOF immediately.
if IFS= read -r _code; then
  echo "unexpected stdin line" >&2
fi
printf '{"status":"ERROR","error":"authentication failed or timed out"}\n'
exit 1
"#,
    );
    cfg.binary = read_stdin_unauth;
    let empty_prompt_inv = Invocation::from_config(
        &cfg,
        dir.path().to_path_buf(),
        String::new(),
        Some("test-model".to_owned()),
        Some(Effort::Low),
        None,
    );
    let started = Instant::now();
    let eof_turn = run_invocation(&cfg, &empty_prompt_inv, &|| false);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(
        matches!(
            eof_turn.error,
            Some(RunError::Refusal(Refusal::Login { .. }))
        ),
        "{eof_turn:?}"
    );
}

#[test]
fn host_mode_isolates_home_for_env_and_file_credential_modes() {
    let dir = TempDir::new().unwrap();
    let seen_home_file = dir.path().join("seen-home");
    let fixture = write_executable(
        dir.path(),
        "ag-print-home",
        &format!(
            "#!/bin/sh\ncat >/dev/null\nprintf '%s' \"$HOME\" > \"{}\"\nprintf '{{\"type\":\"result\",\"result\":\"ok\"}}\\n'\n",
            seen_home_file.display()
        ),
    );
    let real_home = std::env::var_os("HOME").map(PathBuf::from);
    for mode in [CredentialMode::Env, CredentialMode::File] {
        let mut cfg = base_config(fixture.clone());
        cfg.host_mode = true;
        cfg.credential_mode = mode;
        cfg.credential_file = None;
        cfg.credential_env = None;
        let mut inv = Invocation::from_config(
            &cfg,
            dir.path().to_path_buf(),
            "check home".to_owned(),
            Some("test-model".to_owned()),
            Some(Effort::Low),
            None,
        );
        inv.scratch_dir = None;
        let turn = run_invocation(&cfg, &inv, &|| false);
        assert_eq!(turn.error, None, "mode={mode:?} turn={turn:?}");
        let seen_home = PathBuf::from(fs::read_to_string(&seen_home_file).unwrap());
        assert_ne!(
            Some(seen_home.clone()),
            real_home,
            "host mode must isolate HOME for credential mode {mode:?}"
        );
        assert!(
            seen_home
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("antigravity-host-home-")),
            "unexpected host HOME: {}",
            seen_home.display()
        );
        assert!(
            !seen_home.exists(),
            "isolated host HOME should be cleaned up after the turn: {}",
            seen_home.display()
        );
    }
}
