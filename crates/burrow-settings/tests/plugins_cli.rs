use burrow_github::Author;
use burrow_settings::Config;
use burrow_settings::plugins::{JOB_OR_MODEL_ENV, sha256_file};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn clean_cli() -> Command {
    let mut cmd = burrow_test::command(burrow_test::program!("burrow-settings"));
    for var in JOB_OR_MODEL_ENV {
        cmd.env_remove(var);
    }
    cmd
}

fn fake_gh(dir: &Path, name: &str, id: u64, login: &str) -> PathBuf {
    let path = dir.join(name);
    let script = format!(
        r#"#!/bin/sh
printf 'HTTP/2.0 200 OK\r\nContent-Type: application/json\r\n\r\n{{"id":{id},"login":"{login}"}}\n'
"#
    );
    burrow_test::executable(&path, script);
    path
}

fn fake_bwrap(dir: &Path) -> PathBuf {
    let path = dir.join("fake-bwrap");
    let script = r#"#!/bin/sh
set -eu
target=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --ro-bind)
      if [ "$3" = "/plugin/bin" ]; then target="$2"; fi
      shift 3
      ;;
    --ro-bind-try|--setenv) shift 3 ;;
    --tmpfs|--proc|--dev|--chdir) shift 2 ;;
    --unshare-all|--die-with-parent|--new-session|--clearenv) shift ;;
    --) shift; break ;;
    *) shift ;;
  esac
done
exec env -i PATH="/bin:/usr/bin" "$target"
"#;
    burrow_test::executable(&path, script);
    path
}

fn plugin_fixture(path: &Path) -> String {
    let script = r#"#!/bin/sh
set -eu
IFS= read -r req || true
case "$req" in
  *'"kind":"describe"'*)
    printf '%s\n' '{"contract":"conary-plugin v1","by":{"engine":"third","session":null},"kind":"describe","versions":["conary-plugin v1","conary-plugin v2","conary-plugin v3","conary-plugin v4","conary-plugin v5"],"kinds":[],"v2_kinds":["job"],"v4_kinds":[],"models":[{"id":"cli-m1","efforts":["low","high"]}],"capabilities":{"sandbox":"none","network":true,"devices":[],"mcp":false},"meters":[]}'
    ;;
  *)
    printf '%s\n' '{"contract":"conary-plugin v5","by":{"engine":"third","session":null},"execution":{"model":"cli-m1","effort":null,"context":{"kind":"fresh"}},"kind":"job","summary":"ok","next":{"step":"done"}}'
    ;;
esac
"#;
    burrow_test::executable(path, script);
    sha256_file(path).unwrap()
}

#[test]
fn cli_write_door_authenticates_owner_by_id_and_rejects_jobs_and_non_owners() {
    let dir = tempfile::tempdir().unwrap();
    let bwrap = fake_bwrap(dir.path());
    let gh_owner = fake_gh(dir.path(), "gh-owner", 42, "sam");
    let gh_impostor = fake_gh(dir.path(), "gh-impostor", 99, "sam");
    let plugin = dir.path().join("third-plugin");
    let hash = plugin_fixture(&plugin);

    let mut cfg = Config {
        overlay: dir.path().join("settings/overlay.json"),
        engines: vec!["claude".into(), "claude-api".into()],
        owners: vec![Author {
            login: "sam".into(),
            id: 42,
        }],
        plugins_dir: Some(dir.path().join("settings/plugins")),
        plugin_fallback: Some("claude".into()),
        plugin_sandbox: Some(bwrap),
        auth_gh: Some(gh_owner.clone()),
        ..Default::default()
    };
    let cfg_file = dir.path().join("settings.json");
    fs::write(&cfg_file, serde_json::to_vec(&cfg).unwrap()).unwrap();

    let install_args = [
        "plugin",
        "install",
        "third",
        "release",
        plugin.to_str().unwrap(),
        "1.0.0",
        &hash,
    ];

    // 1. Every job/model environment marker fails closed before auth or execution.
    for var in JOB_OR_MODEL_ENV {
        let out = clean_cli()
            .env(var, "1")
            .arg(&cfg_file)
            .args(install_args)
            .output()
            .unwrap();
        assert!(!out.status.success(), "{var} must fail");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("owner-only") && stderr.contains(var),
            "{var}: {stderr}"
        );
    }

    // 2. Non-owner with the same login but wrong numeric account ID fails.
    cfg.auth_gh = Some(gh_impostor);
    fs::write(&cfg_file, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let out = clean_cli()
        .arg(&cfg_file)
        .args(install_args)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not an authorized owner"), "{stderr}");

    // 3. Authenticated owner succeeds, picker shows plugin, verify succeeds, remove succeeds.
    cfg.auth_gh = Some(gh_owner);
    fs::write(&cfg_file, serde_json::to_vec(&cfg).unwrap()).unwrap();
    let out = clean_cli()
        .arg(&cfg_file)
        .args(install_args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Installed engine plugin third 1.0.0"),
        "{stdout}"
    );

    let picker = clean_cli()
        .arg(&cfg_file)
        .args(["plugin", "picker"])
        .output()
        .unwrap();
    assert!(picker.status.success());
    let picker_json: serde_json::Value = serde_json::from_slice(&picker.stdout).unwrap();
    assert_eq!(picker_json["engines"]["third"]["version"], "1.0.0");

    let verify = clean_cli()
        .arg(&cfg_file)
        .args(["plugin", "verify", "third"])
        .output()
        .unwrap();
    assert!(verify.status.success());

    let remove = clean_cli()
        .arg(&cfg_file)
        .args(["plugin", "remove", "third"])
        .output()
        .unwrap();
    assert!(remove.status.success());
}
