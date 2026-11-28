//! Regression tests for live-installed engine plugins:
//! - `Sandbox::Brings` remains runnable on the host while `Sandbox::Needs` is
//!   rejected at install/update time and still refused before execution as
//!   defense in depth on both host and VM turns.
//! - Removing an installed plugin routes persisted job pins (from `--engine`,
//!   inbox labels, or queued turn identities) to the declared fallback instead
//!   of failing.
//! - Updating an installed plugin to withdraw a model or effort recorded in a
//!   job's request fails the turn rather than silently dropping the override.

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

fn write_plugin_script(
    src: &Path,
    engine: &str,
    sandbox: &str,
    models_json: &str,
    canary: Option<&Path>,
) -> String {
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
    sha256_file(src).unwrap()
}

fn install_fixture(
    cfg: &Config,
    src: &Path,
    engine: &str,
    sandbox: &str,
    models_json: &str,
    canary: Option<&Path>,
) {
    let hash = write_plugin_script(src, engine, sandbox, models_json, canary);
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

fn update_fixture(
    cfg: &Config,
    src: &Path,
    engine: &str,
    version: &str,
    sandbox: &str,
    models_json: &str,
) {
    let hash = write_plugin_script(src, engine, sandbox, models_json, None);
    cfg.settings
        .as_ref()
        .unwrap()
        .command_as(
            &owner_auth(),
            &[
                "plugin",
                "update",
                engine,
                "release",
                src.to_str().unwrap(),
                version,
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

    // 2. Installing a plugin declaring `sandbox: needs` is rejected at install
    //    time because live-installed plugins have no VM capability approval path.
    let needs_canary = tmp.path().join("needs.canary");
    let needs_src = tmp.path().join("needs-plugin");
    let needs_hash = write_plugin_script(
        &needs_src,
        "needs-eng",
        "needs",
        r#"[{"id":"n1","efforts":["low"]}]"#,
        Some(&needs_canary),
    );
    let err = cfg
        .settings
        .as_ref()
        .unwrap()
        .command_as(
            &owner_auth(),
            &[
                "plugin",
                "install",
                "needs-eng",
                "release",
                needs_src.to_str().unwrap(),
                "1.0.0",
                &needs_hash,
            ]
            .map(String::from),
        )
        .unwrap_err();
    assert!(err.contains("Sandbox::Needs"), "{err}");
    assert!(
        cfg.settings
            .as_ref()
            .unwrap()
            .installed_plugin("needs-eng")
            .unwrap()
            .is_none()
    );

    // 3. Defense in depth: even if an overlay record were edited to carry
    //    `sandbox: "needs"`, `burrow-proj` still refuses to launch it on BOTH
    //    host and VM turns before executing the plugin binary.
    let roles_file = cfg.settings.as_ref().unwrap().roles_file();
    let mut doc: Value = serde_json::from_str(&fs::read_to_string(&roles_file).unwrap()).unwrap();
    let brings_entry = doc["plugins"]["brings-eng"].clone();
    let mut needs_entry = brings_entry;
    needs_entry["capabilities"]["sandbox"] = serde_json::json!("needs");
    doc["plugins"]["needs-eng"] = needs_entry;
    fs::write(&roles_file, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
    let _ = fs::remove_file(&needs_canary);

    let (_, approved) = cfg.resolve_plugin("needs-eng", None).unwrap();
    assert_eq!(approved, Some(Vec::new()));
    let (_, vm_approved) = cfg.resolve_plugin("needs-eng", Some("vm-1")).unwrap();
    assert_eq!(vm_approved, Some(Vec::new()));

    let job_needs = make_job(&cfg, &project, "job-needs");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("needs-eng".into()),
        model: Some("b1".into()),
        ..Default::default()
    }
    .save(&job_needs)
    .unwrap();
    let needs_choice = crate::engine::choice(&cfg, &job_needs).unwrap();
    let waitdir2 = job_needs.turn_file("1", TurnFile::Waits);
    fs::create_dir_all(&waitdir2).unwrap();
    let prompt2 = job_needs.prompt("1");
    fs::write(&prompt2, "Should refuse on host and VM\n").unwrap();
    for agent in [None, Some("vm-1")] {
        let refused = match spawn(
            &cfg,
            &job_needs,
            "1",
            &prompt2,
            &waitdir2,
            agent,
            true,
            "",
            &needs_choice,
        ) {
            Err(SpawnError::Refused(r)) => r,
            Err(SpawnError::Failed(e)) => {
                panic!("expected Misplaced refusal for {agent:?}, got Failed({e})")
            }
            Ok(_) => panic!("needs-sandbox plugin must not launch for {agent:?}"),
        };
        assert_eq!(refused.kind, RefusalKind::Misplaced);
    }
    assert!(
        !needs_canary.exists(),
        "needs-sandbox plugin must not execute"
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
        override_engine: Some("third".into()),
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

#[test]
fn updating_installed_plugin_to_withdraw_job_override_fails_instead_of_falling_back() {
    let tmp = TempDir::new().unwrap();
    let (mut cfg, project) = setup(&tmp);
    let bwrap = fake_bwrap(&tmp);
    enable_plugin_settings(&mut cfg, &tmp, bwrap);

    let third_v1 = tmp.path().join("third-v1");
    install_fixture(
        &cfg,
        &third_v1,
        "third",
        "none",
        r#"[{"id":"m1","efforts":["low","high"]},{"id":"m2","efforts":["medium"]}]"#,
        None,
    );
    let solo_v1 = tmp.path().join("solo-v1");
    install_fixture(
        &cfg,
        &solo_v1,
        "solo",
        "none",
        r#"[{"id":"only","efforts":["high"]}]"#,
        None,
    );
    let settings = cfg.settings.as_ref().unwrap();
    settings
        .command(&["role", "work", "third"].map(String::from), "host")
        .unwrap();
    settings
        .command(
            &["model", "work", "third", "m2", "medium"].map(String::from),
            "host",
        )
        .unwrap();

    // Job 1: pinned `--engine third --model m1 --effort high`.
    let job_pinned = make_job(&cfg, &project, "job-update-pinned");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("third".into()),
        model: Some("m1".into()),
        effort: Some("high".into()),
        override_engine: Some("third".into()),
        ..Default::default()
    }
    .save(&job_pinned)
    .unwrap();

    // Job 2: unpinned `--role work --model m1 --effort high`.
    let job_role = make_job(&cfg, &project, "job-update-role");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        role: Some("work".into()),
        model: Some("m1".into()),
        effort: Some("high".into()),
        override_engine: Some("third".into()),
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

    // Job 3: queued turn with captured Identity for pinned job.
    let job_queued = make_job(&cfg, &project, "job-update-queued");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("third".into()),
        model: Some("m1".into()),
        effort: Some("high".into()),
        override_engine: Some("third".into()),
        ..Default::default()
    }
    .save(&job_queued)
    .unwrap();
    crate::retry::Identity::capture_choice(
        &job_queued,
        "1",
        &Choice {
            engine: "third".into(),
            model: Some("m1".into()),
            effort: Some("high".into()),
        },
    )
    .unwrap();

    // Job 4: sandboxed (`--vm`) job with per-job model/effort override.
    let job_vm = make_job(&cfg, &project, "job-update-vm");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        agent: Some("vm-1".into()),
        agent_engine: Some("third".into()),
        model: Some("m1".into()),
        effort: Some("high".into()),
        override_engine: Some("third".into()),
        ..Default::default()
    }
    .save(&job_vm)
    .unwrap();

    // Job 5: effort-only override on single-model plugin `solo`.
    let job_effort_only = make_job(&cfg, &project, "job-update-effort-only");
    crate::job::Request {
        project: project.to_string_lossy().into_owned(),
        engine: Some("solo".into()),
        effort: Some("high".into()),
        override_engine: Some("solo".into()),
        ..Default::default()
    }
    .save(&job_effort_only)
    .unwrap();

    // All five resolve cleanly on v1 before the update.
    assert_eq!(
        crate::engine::choice(&cfg, &job_pinned).unwrap(),
        Choice {
            engine: "third".into(),
            model: Some("m1".into()),
            effort: Some("high".into()),
        }
    );
    assert_eq!(
        crate::engine::choice(&cfg, &job_role).unwrap(),
        Choice {
            engine: "third".into(),
            model: Some("m1".into()),
            effort: Some("high".into()),
        }
    );
    assert_eq!(
        crate::engine::choice(&cfg, &job_vm).unwrap(),
        Choice {
            engine: "third".into(),
            model: Some("m1".into()),
            effort: Some("high".into()),
        }
    );
    assert_eq!(
        crate::engine::choice(&cfg, &job_effort_only).unwrap(),
        Choice {
            engine: "solo".into(),
            model: None,
            effort: Some("high".into()),
        }
    );

    // Update `third` to v2 dropping effort `high` from `m1` (role `work` uses
    // `m2/medium`, so the update succeeds). Persisted overrides must fail
    // rather than silently falling back to `m2/medium`.
    let third_v2 = tmp.path().join("third-v2");
    update_fixture(
        &cfg,
        &third_v2,
        "third",
        "2.0.0",
        "none",
        r#"[{"id":"m1","efforts":["low"]},{"id":"m2","efforts":["medium"]}]"#,
    );
    for (label, job) in [
        ("pinned", &job_pinned),
        ("role", &job_role),
        ("vm", &job_vm),
    ] {
        let err = crate::engine::choice(&cfg, job).unwrap_err();
        assert!(
            err.contains("m1") && err.contains("high"),
            "{label} job should fail on withdrawn effort: {err}"
        );
    }
    let err = crate::engine::refresh_queued(&cfg, &job_queued, "1").unwrap_err();
    assert!(
        err.contains("m1") && err.contains("high"),
        "queued turn should fail on withdrawn effort: {err}"
    );

    // Update `third` to v3 withdrawing model `m1` completely.
    let third_v3 = tmp.path().join("third-v3");
    update_fixture(
        &cfg,
        &third_v3,
        "third",
        "3.0.0",
        "none",
        r#"[{"id":"m2","efforts":["medium"]}]"#,
    );
    for (label, job) in [
        ("pinned", &job_pinned),
        ("role", &job_role),
        ("vm", &job_vm),
    ] {
        let err = crate::engine::choice(&cfg, job).unwrap_err();
        assert!(
            err.contains("m1"),
            "{label} job should fail on withdrawn model: {err}"
        );
    }

    // Update `solo` to v2 adding a second model so effort-only without a model
    // is no longer valid: must fail instead of silently dropping `--effort`.
    let solo_v2 = tmp.path().join("solo-v2");
    update_fixture(
        &cfg,
        &solo_v2,
        "solo",
        "2.0.0",
        "none",
        r#"[{"id":"only","efforts":["high"]},{"id":"other","efforts":["low"]}]"#,
    );
    let err = crate::engine::choice(&cfg, &job_effort_only).unwrap_err();
    assert!(
        err.contains("requires a model"),
        "effort-only job should fail when plugin gains a second model: {err}"
    );
}
