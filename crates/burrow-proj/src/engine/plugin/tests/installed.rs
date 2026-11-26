//! Regression tests for live-installed engine plugins:
//! - `Sandbox::Brings` remains runnable on the host while `Sandbox::Needs` is
//!   refused before execution.
//! - Removing an installed plugin routes persisted job pins (from `--engine`,
//!   inbox labels, or queued turn identities) to the declared fallback instead
//!   of failing.

use std::fs;
use std::path::{Path, PathBuf};

use burrow_github::Author;
use burrow_settings::plugins::{OwnerAuth, sha256_file};
use tempfile::TempDir;

use super::*;

fn fake_bwrap(tmp: &TempDir) -> PathBuf {
    let bwrap = tmp.path().join("fake-bwrap");
    burrow_test::executable(
        &bwrap,
        r#"#!/bin/sh
set -eu
target=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --ro-bind) if [ "$3" = "/plugin/bin" ]; then target="$2"; fi; shift 3 ;;
    --ro-bind-try|--setenv) shift 3 ;;
    --tmpfs|--proc|--dev|--chdir) shift 2 ;;
    --unshare-all|--die-with-parent|--new-session|--clearenv) shift ;;
    --) shift; break ;;
    *) shift ;;
  esac
done
exec env -i PATH="/bin:/usr/bin" "$target"
"#,
    );
    bwrap
}

fn owner_auth() -> OwnerAuth {
    OwnerAuth {
        id: 42,
        login: "sam".into(),
        actor: "host:id:42:sam".into(),
    }
}

fn enable_plugin_settings(cfg: &mut Config, tmp: &TempDir, bwrap: PathBuf) {
    let settings = cfg.settings.as_mut().unwrap();
    settings.overlay = tmp.path().join("settings/overlay.json");
    settings.engines = vec!["claude".into(), "claude-api".into()];
    settings.owners = vec![Author {
        login: "sam".into(),
        id: 42,
    }];
    settings.plugins_dir = Some(tmp.path().join("settings/plugins"));
    settings.plugin_fallback = Some("claude".into());
    settings.plugin_sandbox = Some(bwrap);
}

fn install_fixture(
    cfg: &Config,
    src: &Path,
    engine: &str,
    sandbox: &str,
    models_json: &str,
    canary: Option<&Path>,
) {
    let canary_line = canary.map_or(String::new(), |p| format!(": > '{}'\n", p.display()));
    let first_model: Option<String> = serde_json::from_str::<Vec<Value>>(models_json)
        .ok()
        .and_then(|v| v.first().and_then(|m| m["id"].as_str()).map(str::to_owned));
    let model_json = match first_model {
        Some(m) => format!(r#""{m}""#),
        None => "null".to_owned(),
    };
    burrow_test::executable(
        src,
        format!(
            r#"#!/bin/sh
set -eu
{canary_line}IFS= read -r req || true
case "$req" in
  *'"kind":"describe"'*)
    printf '%s\n' '{{"contract":"conary-plugin v1","by":{{"engine":"{engine}","session":null}},"kind":"describe","versions":["conary-plugin v1","conary-plugin v2","conary-plugin v3","conary-plugin v4","conary-plugin v5"],"kinds":[],"v2_kinds":["job"],"v4_kinds":[],"models":{models_json},"capabilities":{{"sandbox":"{sandbox}","network":false,"devices":[],"mcp":false}},"meters":[]}}'
    ;;
  *)
    printf '%s\n' '{{"contract":"conary-plugin v5","by":{{"engine":"{engine}","session":"s-{engine}-1"}},"execution":{{"model":{model_json},"effort":null,"context":{{"kind":"fresh"}}}},"kind":"job","summary":"ran on {engine}","next":{{"step":"done"}}}}'
    ;;
esac
"#
        ),
    );
    let hash = sha256_file(src).unwrap();
    cfg.settings
        .as_ref()
        .unwrap()
        .command_as(
            &owner_auth(),
            &[
                "plugin",
                "install",
                engine,
                "release",
                src.to_str().unwrap(),
                "1.0.0",
                &hash,
            ]
            .map(String::from),
        )
        .unwrap();
}

#[test]
fn installed_plugin_with_sandbox_brings_runs_on_host_while_needs_is_refused() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let bwrap = fake_bwrap(&tmp);
    enable_plugin_settings(&mut cfg, &tmp, bwrap);

    // 1. Installed plugin declaring `sandbox: brings` runs cleanly on the host.
    let brings_src = tmp.path().join("brings-plugin");
    install_fixture(
        &cfg,
        &brings_src,
        "brings-eng",
        "brings",
        r#"[{"id":"b1","efforts":["low"]}]"#,
        None,
    );
    let (_, approved) = cfg.resolve_plugin("brings-eng", None).unwrap();
    assert!(
        approved.is_none(),
        "brings-sandbox plugin must not be treated as needing an external sandbox"
    );

    let job = make_job(&cfg, &project, "job-brings");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("brings-eng".into()),
        model: Some("b1".into()),
        ..Default::default()
    }
    .save(&job)
    .unwrap();
    let choice = crate::engine::choice(&cfg, &job).unwrap();
    let waitdir = job.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir).unwrap();
    let prompt = job.prompt("1");
    fs::write(&prompt, "Run on brings-eng\n").unwrap();
    let mut running = spawn(&cfg, &job, "1", &prompt, &waitdir, None, true, "", &choice).unwrap();
    let status = running.child.take().unwrap().wait().unwrap();
    let res = running.finish(status, false, &cfg, &job, "1", 0);
    assert_eq!(res.rc, 0);
    assert_eq!(
        res.last.as_ref().and_then(|v| v["result"].as_str()),
        Some("ran on brings-eng")
    );

    // 2. Installed plugin declaring `sandbox: needs` is still refused on the host
    //    BEFORE executing the plugin binary.
    let needs_canary = tmp.path().join("needs.canary");
    let needs_src = tmp.path().join("needs-plugin");
    install_fixture(
        &cfg,
        &needs_src,
        "needs-eng",
        "needs",
        r#"[{"id":"n1","efforts":["low"]}]"#,
        Some(&needs_canary),
    );
    let _ = fs::remove_file(&needs_canary);
    let (_, approved) = cfg.resolve_plugin("needs-eng", None).unwrap();
    assert_eq!(approved, Some(Vec::new()));

    let job_needs = make_job(&cfg, &project, "job-needs");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("needs-eng".into()),
        model: Some("n1".into()),
        ..Default::default()
    }
    .save(&job_needs)
    .unwrap();
    let needs_choice = crate::engine::choice(&cfg, &job_needs).unwrap();
    let waitdir2 = job_needs.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir2).unwrap();
    let prompt2 = job_needs.prompt("1");
    fs::write(&prompt2, "Should refuse on host\n").unwrap();
    let refused = match spawn(
        &cfg,
        &job_needs,
        "1",
        &prompt2,
        &waitdir2,
        None,
        true,
        "",
        &needs_choice,
    ) {
        Err(SpawnError::Refused(r)) => r,
        Err(SpawnError::Failed(e)) => panic!("expected Misplaced refusal, got Failed({e})"),
        Ok(_) => panic!("needs-sandbox plugin must not launch on the host"),
    };
    assert_eq!(refused.kind, RefusalKind::Misplaced);
    assert!(
        !needs_canary.exists(),
        "needs-sandbox plugin must not execute on host"
    );
}

#[test]
fn removing_installed_plugin_routes_pinned_queued_and_later_turns_to_fallback() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let bwrap = fake_bwrap(&tmp);
    enable_plugin_settings(&mut cfg, &tmp, bwrap);

    let third_src = tmp.path().join("third-plugin");
    install_fixture(
        &cfg,
        &third_src,
        "third",
        "none",
        r#"[{"id":"third-m1","efforts":["high"]}]"#,
        None,
    );
    // Configure role `work` on `third` and a distinct tuning on fallback `claude`.
    let settings = cfg.settings.as_ref().unwrap();
    settings
        .command(
            &["offer", "claude", "models", "claude-m1"].map(String::from),
            "host",
        )
        .unwrap();
    settings
        .command(
            &["model", "*", "claude", "claude-m1", "medium"].map(String::from),
            "host",
        )
        .unwrap();
    settings
        .command(&["role", "work", "third"].map(String::from), "host")
        .unwrap();

    // Job 1: pinned via `--engine third --model third-m1 --effort high` (later turn 2).
    let job_pinned = make_job(&cfg, &project, "job-pinned-later");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("third".into()),
        model: Some("third-m1".into()),
        effort: Some("high".into()),
        ..Default::default()
    }
    .save(&job_pinned)
    .unwrap();
    assert_eq!(
        crate::engine::choice(&cfg, &job_pinned).unwrap(),
        Choice {
            engine: "third".into(),
            model: Some("third-m1".into()),
            effort: Some("high".into()),
        }
    );

    // Job 2: inbox-label pinned (`engine: Some("third")`) with queued turn 1
    // already captured in `retry::Identity`.
    let job_queued = make_job(&cfg, &project, "job-pinned-queued");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("third".into()),
        ..Default::default()
    }
    .save(&job_queued)
    .unwrap();
    crate::retry::Identity::capture_choice(
        &job_queued,
        "1",
        &Choice {
            engine: "third".into(),
            model: None,
            effort: None,
        },
    )
    .unwrap();

    // Job 3: unpinned role job with `--model third-m1 --effort high` whose role
    // moves from `third` to `claude` upon removal.
    let job_role = make_job(&cfg, &project, "job-role-override");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        role: Some("work".into()),
        model: Some("third-m1".into()),
        effort: Some("high".into()),
        ..Default::default()
    }
    .save(&job_role)
    .unwrap();
    crate::policy::bind(
        &job_role,
        crate::policy::Identity::Local,
        crate::policy::Kind::Automated,
    )
    .unwrap();

    // Remove `third` with fallback `claude`.
    settings
        .command_as(
            &owner_auth(),
            &["plugin", "remove", "third", "--fallback", "claude"].map(String::from),
        )
        .unwrap();

    let expected_fallback = Choice {
        engine: "claude".into(),
        model: Some("claude-m1".into()),
        effort: Some("medium".into()),
    };

    // Job 1 (later turn on pinned job) reaches fallback `claude` and drops `third-m1`.
    assert_eq!(
        crate::engine::choice(&cfg, &job_pinned).unwrap(),
        expected_fallback
    );

    // Job 2 (queued turn with captured Identity on removed plugin) reroutes to `claude`.
    assert_eq!(
        crate::engine::choice(&cfg, &job_queued).unwrap(),
        expected_fallback
    );
    let refreshed = crate::engine::refresh_queued(&cfg, &job_queued, "1")
        .unwrap()
        .expect("refreshed queued turn");
    assert_eq!(refreshed, expected_fallback);
    assert_eq!(
        crate::retry::Identity::load(&job_queued, "1")
            .unwrap()
            .unwrap()
            .choice,
        expected_fallback
    );

    // Job 3 (role job with model override for removed plugin) reaches `claude` cleanly.
    assert_eq!(
        crate::engine::choice(&cfg, &job_role).unwrap(),
        expected_fallback
    );
}
