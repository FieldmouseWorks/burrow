use super::*;
use crate::Config;
use crate::door::{DoorConfig, pass};
use burrow_github::Author;
use burrow_github::fake::{Fake, ok};
use serde_json::{Value, json};
use std::fs;

fn words(line: &str) -> Vec<String> {
    line.split_whitespace().map(str::to_owned).collect()
}

fn owner() -> Author {
    Author {
        login: "sam".into(),
        id: 42,
    }
}

fn owner_auth() -> OwnerAuth {
    OwnerAuth {
        id: 42,
        login: "sam".into(),
        actor: "host:id:42:sam".into(),
    }
}

/// Stand-in sandbox runner for unit tests that may run inside a nested Nix
/// build sandbox where `unshare(CLONE_NEWUSER)` is unavailable. Asserts every
/// required bubblewrap confinement flag was passed, wipes the environment, and
/// executes the mounted `/plugin/bin` target.
fn fake_bwrap(dir: &Path, args_log: &Path) -> PathBuf {
    let path = dir.join("fake-bwrap");
    let script = format!(
        r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> {log}
has_unshare=0
has_clearenv=0
target=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --unshare-all) has_unshare=1; shift ;;
    --clearenv) has_clearenv=1; shift ;;
    --ro-bind)
      if [ "$3" = "/plugin/bin" ]; then target="$2"; fi
      shift 3
      ;;
    --ro-bind-try|--setenv) shift 3 ;;
    --tmpfs|--proc|--dev|--chdir) shift 2 ;;
    --die-with-parent|--new-session) shift ;;
    --) shift; break ;;
    *) shift ;;
  esac
done
[ "$has_unshare" = 1 ] || {{ echo "missing --unshare-all" >&2; exit 97; }}
[ "$has_clearenv" = 1 ] || {{ echo "missing --clearenv" >&2; exit 98; }}
[ -n "$target" ] || {{ echo "missing /plugin/bin bind" >&2; exit 99; }}
exec env -i PATH="/bin:/usr/bin" "$target"
"#,
        log = sh_quote(&args_log.to_string_lossy()),
    );
    burrow_test::executable(&path, script);
    path
}

/// Build a conforming `conary-plugin v5` (`job`) shell fixture that declares
/// `engine`, `models_json`, and `sandbox`, optionally writing a canary file
/// whenever executed (to prove tampered artifacts never execute) and optionally
/// rejecting conformance fixtures.
fn write_plugin_fixture(
    path: &Path,
    engine: &str,
    models_json: &str,
    sandbox: &str,
    pass_conformance: bool,
    canary: Option<&Path>,
    forbid_env: Option<&str>,
) -> String {
    let canary_line = canary.map_or(String::new(), |p| {
        format!(": > {}\n", sh_quote(&p.to_string_lossy()))
    });
    let forbid_line = forbid_env.map_or(String::new(), |var| {
        format!("if [ -n \"${{{var}:-}}\" ]; then echo \"leaked {var}\" >&2; exit 91; fi\n")
    });
    let first_model: Option<String> = serde_json::from_str::<Vec<Value>>(models_json)
        .ok()
        .and_then(|v| v.first().and_then(|m| m["id"].as_str()).map(str::to_owned));
    let model_json = match first_model {
        Some(m) => format!(r#""{m}""#),
        None => "null".to_owned(),
    };
    let job_response = if pass_conformance {
        format!(
            r#"{{"contract":"conary-plugin v5","by":{{"engine":"{engine}","session":null}},"execution":{{"model":{model_json},"effort":null,"context":{{"kind":"fresh"}}}},"kind":"job","summary":"conformance ok","next":{{"step":"done"}}}}"#
        )
    } else {
        r#"{"contract":"conary-plugin v5","by":{"engine":"wrong","session":null},"kind":"job","summary":"","next":{"step":"done"}}"#.to_owned()
    };
    let script = format!(
        r#"#!/bin/sh
set -eu
{canary_line}{forbid_line}IFS= read -r req || true
case "$req" in
  *'"kind":"describe"'*)
    printf '%s\n' '{{"contract":"conary-plugin v1","by":{{"engine":"{engine}","session":null}},"kind":"describe","versions":["conary-plugin v1","conary-plugin v2","conary-plugin v3","conary-plugin v4","conary-plugin v5"],"kinds":[],"v2_kinds":["job"],"v4_kinds":[],"models":{models_json},"capabilities":{{"sandbox":"{sandbox}","network":true,"devices":[],"mcp":false}},"meters":["quota"]}}'
    ;;
  *)
    printf '%s\n' '{job_response}'
    ;;
esac
"#
    );
    burrow_test::executable(path, script);
    sha256_file(path).unwrap()
}

fn rig() -> (tempfile::TempDir, Config, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let nix_store = dir.path().join("nix-store");
    fs::create_dir_all(&nix_store).unwrap();
    let args_log = dir.path().join("bwrap.log");
    let bwrap = fake_bwrap(dir.path(), &args_log);
    let cfg = Config {
        overlay: dir.path().join("settings/overlay.json"),
        engines: vec!["claude".into(), "claude-api".into()],
        owners: vec![owner()],
        plugins_dir: Some(dir.path().join("settings/plugins")),
        plugin_fallback: Some("claude".into()),
        plugin_sandbox: Some(bwrap),
        nix_store: Some(nix_store),
        ..Default::default()
    };
    (dir, cfg, args_log)
}

#[test]
fn install_update_schema_picker_audit_and_removal_fallback_for_nix_and_release() {
    let (dir, cfg, args_log) = rig();

    // 1. Install a pinned Nix-built conformance plugin.
    let nix_v1 = cfg.nix_store().join("abc-third-1.0.0-bin");
    let models_v1 = r#"[{"id":"third-m1","efforts":["low","high"],"context_tokens":64000,"max_output_tokens":8192}]"#;
    let hash_v1 = write_plugin_fixture(
        &nix_v1,
        "third",
        models_v1,
        "none",
        true,
        None,
        Some("BURROW_TEST_SECRET"),
    );

    let out = cfg
        .command_as(
            &owner_auth(),
            &words(&format!(
                "plugin install third nix {} 1.0.0 {hash_v1}",
                nix_v1.display()
            )),
        )
        .unwrap();
    assert!(out.contains("Installed engine plugin third 1.0.0"), "{out}");
    assert!(out.contains("third-m1"), "{out}");

    // Sandbox flags were enforced during `cn plugin check`.
    let bwrap_log = fs::read_to_string(&args_log).unwrap();
    assert!(bwrap_log.contains("--unshare-all"), "{bwrap_log}");
    assert!(bwrap_log.contains("--clearenv"), "{bwrap_log}");

    // Immediate schema choices, picker data, and role/model usability.
    let picker = cfg.picker().unwrap();
    let third_picker = picker.engines.get("third").expect("third in picker");
    assert!(third_picker.installed && third_picker.removable);
    assert_eq!(third_picker.version.as_deref(), Some("1.0.0"));
    assert_eq!(third_picker.sha256.as_deref(), Some(hash_v1.as_str()));
    assert_eq!(third_picker.models.len(), 1);
    assert_eq!(third_picker.models[0].id, "third-m1");
    assert_eq!(third_picker.models[0].efforts, vec!["low", "high"]);
    let caps = third_picker.capabilities.as_ref().unwrap();
    assert_eq!(caps.sandbox, "none");
    assert!(caps.network && !caps.mcp);
    assert!(caps.declared.contains(&"workspace".to_owned()));
    assert!(caps.declared.contains(&"network".to_owned()));
    assert!(!caps.declared.contains(&"github".to_owned()));

    let live_schema = cfg.live_schema().unwrap();
    let role_enum = live_schema
        .pointer("/properties/options/properties/proj.roles/properties/value/additionalProperties/properties/engine/enum")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(role_enum.contains(&json!("third")), "{role_enum:?}");
    let job_variants = live_schema
        .pointer("/$defs/jobChoice/oneOf")
        .unwrap()
        .as_array()
        .unwrap();
    let third_variant = job_variants
        .iter()
        .find(|v| v.pointer("/properties/engine/const") == Some(&json!("third")))
        .expect("third in $defs.jobChoice");
    assert_eq!(
        third_variant.pointer("/properties/model/anyOf/1/enum"),
        Some(&json!(["third-m1"]))
    );
    assert_eq!(
        third_variant.pointer("/x-capabilities/sandbox"),
        Some(&json!("none"))
    );

    // Route roles and pick a declared model immediately.
    cfg.command(&words("role work third"), "host").unwrap();
    cfg.command(&words("role triage@sam/box third over 90 claude"), "host")
        .unwrap();
    cfg.command(
        &words("role fix claude over 80 third over 95 claude-api"),
        "host",
    )
    .unwrap();
    cfg.command(&words("model work third third-m1 high"), "host")
        .unwrap();
    cfg.command(&words("engines save third-setup"), "host")
        .unwrap();
    // Undeclared model is rejected.
    assert!(
        cfg.command(&words("model work third bogus-model high"), "host")
            .is_err()
    );

    // 2. Failed update (failed conformance check) preserves v1 active installation.
    let bad_rel = dir.path().join("bad-third-2.0.0");
    let bad_hash = write_plugin_fixture(&bad_rel, "third", models_v1, "none", false, None, None);
    let err = cfg
        .command_as(
            &owner_auth(),
            &words(&format!(
                "plugin update third release {} 2.0.0-bad {bad_hash}",
                bad_rel.display()
            )),
        )
        .unwrap_err();
    assert!(err.contains("cn plugin check"), "{err}");
    let still_v1 = cfg.installed_plugin("third").unwrap().unwrap();
    assert_eq!(still_v1.version, "1.0.0");
    assert_eq!(still_v1.sha256, hash_v1);

    // 3. Valid update to a verified release artifact v2 (retaining third-m1 + adding third-m2).
    let rel_v2 = dir.path().join("third-2.0.0");
    let models_v2 = r#"[{"id":"third-m1","efforts":["low","high"]},{"id":"third-m2","efforts":["medium","max"]}]"#;
    let hash_v2 = write_plugin_fixture(&rel_v2, "third", models_v2, "none", true, None, None);
    let updated = cfg
        .command_as(
            &owner_auth(),
            &words(&format!(
                "plugin update third release {} 2.0.0 {hash_v2}",
                rel_v2.display()
            )),
        )
        .unwrap();
    assert!(
        updated.contains("Updated engine plugin third 2.0.0"),
        "{updated}"
    );
    assert!(updated.contains("third-m2"), "{updated}");
    let v2 = cfg.installed_plugin("third").unwrap().unwrap();
    assert_eq!(v2.source_kind, SourceKind::Release);
    assert_eq!(v2.version, "2.0.0");
    assert_eq!(v2.sha256, hash_v2);
    cfg.verify_installed_plugin("third").unwrap();

    // New model is immediately selectable.
    cfg.command(&words("model work third third-m2 max"), "host")
        .unwrap();

    // 4. Removal atomically moves affected roles and saved setups to fallback and reports moves.
    let removed = cfg
        .command_as(&owner_auth(), &words("plugin remove third"))
        .unwrap();
    assert!(
        removed.contains("Removed engine plugin third 2.0.0"),
        "{removed}"
    );
    assert!(removed.contains("Fallback engine: claude"), "{removed}");
    assert!(removed.contains("- work: third -> claude"), "{removed}");
    assert!(
        removed.contains("- triage@sam/box: third over 90 claude -> claude"),
        "{removed}"
    );
    assert!(
        removed.contains(
            "- fix: claude over 80 third over 95 claude-api -> claude over 95 claude-api"
        ),
        "{removed}"
    );
    assert!(cfg.installed_plugin("third").unwrap().is_none());
    let force = cfg.roles().unwrap();
    assert_eq!(force.route("work", None).text(), "claude");
    assert_eq!(force.route("triage", Some("sam/box")).text(), "claude");
    assert_eq!(force.route("fix", None).text(), "claude over 95 claude-api");
    assert!(!force.offers.contains_key("third"));

    // 5. Audit log recorded actor, actorId, source, version, hash for install, update, remove.
    let log_lines: Vec<Value> = fs::read_to_string(cfg.overlay.with_extension("log"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let plugin_entries: Vec<&Value> = log_lines
        .iter()
        .filter(|v| v.get("plugin").is_some())
        .collect();
    assert_eq!(plugin_entries.len(), 3, "{plugin_entries:?}");
    assert_eq!(plugin_entries[0]["actor"], "host:id:42:sam");
    assert_eq!(plugin_entries[0]["actorId"], 42);
    assert_eq!(plugin_entries[0]["plugin"]["operation"], "install");
    assert_eq!(plugin_entries[0]["plugin"]["version"], "1.0.0");
    assert_eq!(plugin_entries[0]["plugin"]["sha256"], hash_v1);
    assert!(
        plugin_entries[0]["plugin"]["source"]
            .as_str()
            .unwrap()
            .contains("abc-third-1.0.0-bin")
    );

    assert_eq!(plugin_entries[1]["plugin"]["operation"], "update");
    assert_eq!(plugin_entries[1]["plugin"]["version"], "2.0.0");
    assert_eq!(plugin_entries[1]["plugin"]["sha256"], hash_v2);
    assert_eq!(plugin_entries[1]["plugin"]["previousVersion"], "1.0.0");
    assert_eq!(plugin_entries[1]["plugin"]["previousSha256"], hash_v1);

    assert_eq!(plugin_entries[2]["plugin"]["operation"], "remove");
    assert_eq!(plugin_entries[2]["plugin"]["version"], "2.0.0");
    assert_eq!(plugin_entries[2]["plugin"]["sha256"], hash_v2);
    assert_eq!(plugin_entries[2]["plugin"]["fallback"], "claude");
    assert_eq!(
        plugin_entries[2]["plugin"]["movedRoles"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
}

#[test]
fn non_owner_unauthenticated_and_job_originated_installs_fail() {
    let (dir, cfg, _) = rig();
    let plugin = dir.path().join("third");
    let hash = write_plugin_fixture(
        &plugin,
        "third",
        r#"[{"id":"m1","efforts":["low"]}]"#,
        "none",
        true,
        None,
        None,
    );
    let args = words(&format!(
        "plugin install third release {} 1.0.0 {hash}",
        plugin.display()
    ));

    // Direct unauthenticated Config::command is always refused.
    let err = cfg.command(&args, "host").unwrap_err();
    assert!(err.contains("authenticated owner"), "{err}");

    // Matching login with wrong account ID is refused (#660).
    let spoofed = OwnerAuth {
        id: 999,
        login: "sam".into(),
        actor: "host:id:999:sam".into(),
    };
    let err = cfg.command_as(&spoofed, &args).unwrap_err();
    assert!(err.contains("not an authorized owner"), "{err}");

    // Empty owners list fails closed.
    let mut no_owners = cfg.clone();
    no_owners.owners.clear();
    let err = no_owners.command_as(&owner_auth(), &args).unwrap_err();
    assert!(err.contains("no authorized owners"), "{err}");

    assert!(cfg.installed_plugins().unwrap().is_empty());
}

#[test]
fn tampered_artifacts_fail_before_execution_and_at_subsequent_start() {
    let (dir, cfg, _) = rig();
    let canary = dir.path().join("executed.canary");
    let plugin = dir.path().join("third-rel");
    let real_hash = write_plugin_fixture(
        &plugin,
        "third",
        r#"[{"id":"m1","efforts":["low"]}]"#,
        "none",
        true,
        Some(&canary),
        None,
    );
    let wrong_hash = format!("sha256:{}", "0".repeat(64));

    // Wrong hash fails BEFORE executing the artifact.
    let err = cfg
        .command_as(
            &owner_auth(),
            &words(&format!(
                "plugin install third release {} 1.0.0 {wrong_hash}",
                plugin.display()
            )),
        )
        .unwrap_err();
    assert!(err.contains("hash mismatch"), "{err}");
    assert!(
        !canary.exists(),
        "tampered artifact was executed before hash verification!"
    );

    // Install with the real hash succeeds.
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install third release {} 1.0.0 {real_hash}",
            plugin.display()
        )),
    )
    .unwrap();
    let installed = cfg.installed_plugin("third").unwrap().unwrap();
    assert!(cfg.verify_installed_plugin("third").is_ok());

    // Tamper with the stored release artifact on disk after installation.
    fs::remove_file(&installed.path).unwrap();
    burrow_test::executable(&installed.path, "#!/bin/sh\necho pwned\n");
    let err = cfg.verify_installed_plugin("third").unwrap_err();
    assert!(err.contains("verification failed"), "{err}");
}

#[test]
fn removal_refuses_missing_or_invalid_fallback() {
    let (dir, mut cfg, _) = rig();
    cfg.plugin_fallback = None;
    let plugin = dir.path().join("third");
    let hash = write_plugin_fixture(
        &plugin,
        "third",
        r#"[{"id":"m1","efforts":["low"]}]"#,
        "none",
        true,
        None,
        None,
    );
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install third release {} 1.0.0 {hash}",
            plugin.display()
        )),
    )
    .unwrap();
    cfg.command(&words("role work third"), "host").unwrap();

    for bad in [
        "plugin remove third",
        "plugin remove third --fallback third",
        "plugin remove third --fallback ghost",
        "plugin remove third --fallback codex",
    ] {
        assert!(
            cfg.command_as(&owner_auth(), &words(bad)).is_err(),
            "{bad} should be refused"
        );
        assert!(cfg.installed_plugin("third").unwrap().is_some());
        assert_eq!(cfg.roles().unwrap().route("work", None).text(), "third");
    }

    // Explicit valid --fallback succeeds.
    let ok_msg = cfg
        .command_as(
            &owner_auth(),
            &words("plugin remove third --fallback claude"),
        )
        .unwrap();
    assert!(ok_msg.contains("- work: third -> claude"), "{ok_msg}");
    assert!(cfg.installed_plugin("third").unwrap().is_none());
}

#[test]
fn github_settings_door_authenticates_owner_by_account_id_for_plugins() {
    let (dir, mut cfg, _) = rig();
    cfg.owners = vec![burrow_github::fake::author("sam")];
    let plugin = dir.path().join("third-door");
    let hash = write_plugin_fixture(
        &plugin,
        "third",
        r#"[{"id":"door-m1","efforts":["low"]}]"#,
        "none",
        true,
        None,
        None,
    );
    let ask = format!(
        "plugin install third release {} 1.0.0 {hash}",
        plugin.display()
    );

    let make_gh = |opener_login: &str, opener_id: u64, editor: Option<(&str, u64)>| {
        let mut gh = Fake::default();
        let actor = |login: &str, id: u64| json!({"login": login, "id": id, "databaseId": id});
        let edits: Vec<_> = editor
            .into_iter()
            .map(|(l, i)| json!({"editor": actor(l, i), "deletedBy": null}))
            .collect();
        gh.on(
            "GET",
            "repos/sam/box/issues",
            vec![ok(
                200,
                json!([{"number": 7, "user": actor(opener_login, opener_id)}]),
                &[],
            )],
        )
        .on(
            "POST",
            "graphql",
            vec![ok(
                200,
                json!({"data":{"repository":{"issue":{
                    "id":"iss-7",
                    "author": actor(opener_login, opener_id),
                    "title":"settings",
                    "body": ask,
                    "openCycle":{"nodes":[]},
                    "userContentEdits":{"totalCount":edits.len(),"nodes":edits},
                    "timelineItems":{"totalCount":0,"nodes":[]}
                }}}}),
                &[],
            )],
        )
        .on(
            "POST",
            "repos/sam/box/issues/7/comments",
            vec![ok(201, json!({}), &[])],
        )
        .on(
            "PATCH",
            "repos/sam/box/issues/7",
            vec![ok(200, json!({}), &[])],
        );
        gh
    };

    let door_cfg = DoorConfig {
        repo: "sam/box".into(),
        label: "settings".into(),
        authors: vec![burrow_github::fake::author("sam")],
        state: dir.path().join("door"),
        settings: cfg.clone(),
    };
    let sam_id = burrow_github::fake::author("sam").id;

    // Non-owner with same login but wrong account ID is rejected.
    pass(&mut make_gh("sam", sam_id + 1, None), &door_cfg).unwrap();
    assert!(cfg.installed_plugin("third").unwrap().is_none());

    // Owner issue edited by outsider is rejected.
    pass(
        &mut make_gh("sam", sam_id, Some(("outsider", 9999))),
        &door_cfg,
    )
    .unwrap();
    assert!(cfg.installed_plugin("third").unwrap().is_none());

    // Authentic owner account ID installs the plugin and logs github actor + id.
    pass(&mut make_gh("sam", sam_id, None), &door_cfg).unwrap();
    let installed = cfg.installed_plugin("third").unwrap().unwrap();
    assert_eq!(installed.version, "1.0.0");
    assert!(installed.actor.contains(&format!("id:{sam_id}:sam")));
}

#[test]
fn model_specific_efforts_are_enforced_in_roles_updates_and_live_schema() {
    let (dir, cfg, _) = rig();

    let plugin_v1 = dir.path().join("third-v1");
    let models_v1 = r#"[{"id":"m1","efforts":["low"]},{"id":"m2","efforts":["high","max"]},{"id":"m3","efforts":[]}]"#;
    let hash_v1 = write_plugin_fixture(&plugin_v1, "third", models_v1, "none", true, None, None);
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install third release {} 1.0.0 {hash_v1}",
            plugin_v1.display()
        )),
    )
    .unwrap();

    // Valid per-model pairs succeed.
    cfg.command(&words("model work third m1 low"), "host")
        .unwrap();
    cfg.command(&words("model fix third m2 max"), "host")
        .unwrap();
    cfg.command(&words("model judge third m3 -"), "host")
        .unwrap();

    // Cross-model effort pairs and effort-without-model on a multi-model plugin are refused.
    let err = cfg
        .command(&words("model work third m1 high"), "host")
        .unwrap_err();
    assert!(err.contains("m1") && err.contains("high"), "{err}");
    assert!(
        cfg.command(&words("model work third m2 low"), "host")
            .is_err()
    );
    assert!(
        cfg.command(&words("model work third m3 low"), "host")
            .is_err()
    );
    let err = cfg
        .command(&words("model work third - low"), "host")
        .unwrap_err();
    assert!(err.contains("requires a model"), "{err}");

    let force = cfg.roles().unwrap();
    assert!(
        force
            .validate_choice("third", Some("m1"), Some("low"))
            .is_ok()
    );
    assert!(
        force
            .validate_choice("third", Some("m1"), Some("high"))
            .is_err()
    );
    assert!(force.validate_choice("third", None, Some("low")).is_err());

    // Updating the plugin to drop "low" from m1 (even while moving "low" onto m2)
    // is rejected because role `work` is tuned to (m1, low), preserving v1.
    let plugin_v2_bad = dir.path().join("third-v2-bad");
    let models_v2_bad = r#"[{"id":"m1","efforts":["medium"]},{"id":"m2","efforts":["low","high","max"]},{"id":"m3","efforts":[]}]"#;
    let hash_v2_bad = write_plugin_fixture(
        &plugin_v2_bad,
        "third",
        models_v2_bad,
        "none",
        true,
        None,
        None,
    );
    let err = cfg
        .command_as(
            &owner_auth(),
            &words(&format!(
                "plugin update third release {} 2.0.0 {hash_v2_bad}",
                plugin_v2_bad.display()
            )),
        )
        .unwrap_err();
    assert!(err.contains("m1") && err.contains("low"), "{err}");
    assert_eq!(
        cfg.installed_plugin("third").unwrap().unwrap().version,
        "1.0.0"
    );

    // Live schema `$defs.jobChoice` preserves exact per-model effort pairs via `anyOf`.
    let schema = cfg.live_schema().unwrap();
    let variants = schema
        .pointer("/$defs/jobChoice/oneOf")
        .unwrap()
        .as_array()
        .unwrap();
    let third = variants
        .iter()
        .find(|v| v.pointer("/properties/engine/const") == Some(&json!("third")))
        .expect("third variant");
    let pairs = third["anyOf"].as_array().expect("per-model anyOf pairs");
    assert_eq!(pairs.len(), 4, "{pairs:?}");
    // Multi-model plugin requires effort to be null when model is null/omitted.
    assert_eq!(pairs[0]["properties"]["model"], json!({"type": "null"}));
    assert_eq!(pairs[0]["properties"]["effort"], json!({"type": "null"}));
    // Each model branch restricts effort to that model's declared efforts alone.
    let branch = |id: &str| {
        pairs
            .iter()
            .find(|p| p.pointer("/properties/model/const") == Some(&json!(id)))
            .unwrap_or_else(|| panic!("missing branch for {id}: {pairs:?}"))
    };
    assert_eq!(
        branch("m1").pointer("/properties/effort/anyOf/1/enum"),
        Some(&json!(["low"]))
    );
    assert_eq!(
        branch("m2").pointer("/properties/effort/anyOf/1/enum"),
        Some(&json!(["high", "max"]))
    );
    assert_eq!(
        branch("m3").pointer("/properties/effort"),
        Some(&json!({"type": "null"}))
    );

    // Single-model plugin allows selecting its efforts without repeating the model.
    let single = dir.path().join("single-v1");
    let single_hash = write_plugin_fixture(
        &single,
        "solo",
        r#"[{"id":"only","efforts":["low"]}]"#,
        "none",
        true,
        None,
        None,
    );
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install solo release {} 1.0.0 {single_hash}",
            single.display()
        )),
    )
    .unwrap();
    let force = cfg.roles().unwrap();
    assert!(force.validate_choice("solo", None, Some("low")).is_ok());
    assert!(force.validate_choice("solo", None, Some("high")).is_err());
    let schema = cfg.live_schema().unwrap();
    let solo = schema
        .pointer("/$defs/jobChoice/oneOf")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v.pointer("/properties/engine/const") == Some(&json!("solo")))
        .expect("solo variant");
    assert_eq!(
        solo.pointer("/anyOf/0/properties/effort/anyOf/1/enum"),
        Some(&json!(["low"]))
    );
}

#[test]
fn removal_records_fallbacks_including_transitive_chains_and_clears_on_reinstall() {
    let (dir, cfg, _) = rig();

    let third_bin = dir.path().join("third");
    let third_hash = write_plugin_fixture(
        &third_bin,
        "third",
        r#"[{"id":"m3","efforts":["low"]}]"#,
        "none",
        true,
        None,
        None,
    );
    let fourth_bin = dir.path().join("fourth");
    let fourth_hash = write_plugin_fixture(
        &fourth_bin,
        "fourth",
        r#"[{"id":"m4","efforts":["high"]}]"#,
        "none",
        true,
        None,
        None,
    );
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install third release {} 1.0.0 {third_hash}",
            third_bin.display()
        )),
    )
    .unwrap();
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install fourth release {} 1.0.0 {fourth_hash}",
            fourth_bin.display()
        )),
    )
    .unwrap();

    // Remove fourth with fallback third.
    cfg.command_as(
        &owner_auth(),
        &words("plugin remove fourth --fallback third"),
    )
    .unwrap();
    let force = cfg.roles().unwrap();
    assert_eq!(force.fallback_for("fourth"), Some("third"));
    assert_eq!(force.fallback_for("third"), None);

    // Removing third with fallback claude updates fourth's transitive fallback too.
    cfg.command_as(
        &owner_auth(),
        &words("plugin remove third --fallback claude"),
    )
    .unwrap();
    let force = cfg.roles().unwrap();
    assert_eq!(force.fallback_for("third"), Some("claude"));
    assert_eq!(force.fallback_for("fourth"), Some("claude"));

    // Reinstalling third clears third's removal fallback while preserving fourth's.
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install third release {} 1.0.0 {third_hash}",
            third_bin.display()
        )),
    )
    .unwrap();
    let force = cfg.roles().unwrap();
    assert_eq!(force.fallback_for("third"), None);
    assert_eq!(force.fallback_for("fourth"), Some("claude"));
}

#[test]
fn sandbox_needs_plugins_are_rejected_at_install_and_update_while_brings_succeeds() {
    let (dir, cfg, _) = rig();

    // 1. Installing a plugin declaring `sandbox: needs` fails closed and never
    //    appears in installed plugins, picker data, or live JSON Schema.
    let needs_bin = dir.path().join("needs-v1");
    let needs_hash = write_plugin_fixture(
        &needs_bin,
        "needs-eng",
        r#"[{"id":"n1","efforts":["low"]}]"#,
        "needs",
        true,
        None,
        None,
    );
    let err = cfg
        .command_as(
            &owner_auth(),
            &words(&format!(
                "plugin install needs-eng release {} 1.0.0 {needs_hash}",
                needs_bin.display()
            )),
        )
        .unwrap_err();
    assert!(err.contains("Sandbox::Needs"), "{err}");
    assert!(err.contains("burrow.agents.engines.needs-eng"), "{err}");
    assert!(cfg.installed_plugin("needs-eng").unwrap().is_none());
    assert!(!cfg.picker().unwrap().engines.contains_key("needs-eng"));
    let schema = cfg.live_schema().unwrap();
    let variants = schema
        .pointer("/$defs/jobChoice/oneOf")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(
        !variants
            .iter()
            .any(|v| v.pointer("/properties/engine/const") == Some(&json!("needs-eng")))
    );
    assert!(!cfg.plugins_dir().join("needs-eng").exists());

    // 2. Installing a plugin declaring `sandbox: brings` succeeds.
    let brings_v1 = dir.path().join("brings-v1");
    let brings_hash = write_plugin_fixture(
        &brings_v1,
        "brings-eng",
        r#"[{"id":"b1","efforts":["low"]}]"#,
        "brings",
        true,
        None,
        None,
    );
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install brings-eng release {} 1.0.0 {brings_hash}",
            brings_v1.display()
        )),
    )
    .unwrap();
    assert_eq!(
        cfg.installed_plugin("brings-eng")
            .unwrap()
            .unwrap()
            .capabilities
            .sandbox,
        "brings"
    );

    // 3. Updating that plugin to a v2 declaring `sandbox: needs` is rejected
    //    and preserves the prior `brings` v1 installation untouched.
    let brings_v2_needs = dir.path().join("brings-v2-needs");
    let v2_hash = write_plugin_fixture(
        &brings_v2_needs,
        "brings-eng",
        r#"[{"id":"b1","efforts":["low"]}]"#,
        "needs",
        true,
        None,
        None,
    );
    let err = cfg
        .command_as(
            &owner_auth(),
            &words(&format!(
                "plugin update brings-eng release {} 2.0.0 {v2_hash}",
                brings_v2_needs.display()
            )),
        )
        .unwrap_err();
    assert!(err.contains("Sandbox::Needs"), "{err}");
    let kept = cfg.installed_plugin("brings-eng").unwrap().unwrap();
    assert_eq!(kept.version, "1.0.0");
    assert_eq!(kept.sha256, brings_hash);
    assert_eq!(kept.capabilities.sandbox, "brings");
}
