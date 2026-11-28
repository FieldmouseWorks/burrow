use super::tests::{owner, owner_auth, rig, words, write_plugin_fixture};
use crate::door::{DoorConfig, pass};
use burrow_github::Author;
use burrow_github::fake::{Fake, ok};
use serde_json::{Value, json};
use std::fs;

fn actor_json(login: &str, id: u64) -> Value {
    json!({"login": login, "id": id, "databaseId": id})
}

fn issue_graphql(
    num: u64,
    author: (&str, u64),
    title: &str,
    body: &str,
    edits: Vec<Value>,
    renames: Vec<Value>,
) -> Value {
    json!({
        "data": {
            "repository": {
                "issue": {
                    "id": format!("iss-{num}"),
                    "author": actor_json(author.0, author.1),
                    "title": title,
                    "body": body,
                    "openCycle": {"nodes": []},
                    "userContentEdits": {"totalCount": edits.len(), "nodes": edits},
                    "timelineItems": {"totalCount": renames.len(), "nodes": renames}
                }
            }
        }
    })
}

fn fake_door_gh(num: u64, opener: (&str, u64), gql_answers: Vec<Value>) -> Fake {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/box/issues",
        vec![ok(
            200,
            json!([{"number": num, "user": actor_json(opener.0, opener.1)}]),
            &[],
        )],
    )
    .on(
        "POST",
        "graphql",
        gql_answers.into_iter().map(|v| ok(200, v, &[])).collect(),
    )
    .on(
        "POST",
        &format!("repos/sam/box/issues/{num}/comments"),
        vec![ok(201, json!({}), &[])],
    )
    .on(
        "PATCH",
        &format!("repos/sam/box/issues/{num}"),
        vec![ok(200, json!({}), &[])],
    );
    gh
}

fn posted_comment(gh: &Fake) -> String {
    gh.asked("POST")
        .into_iter()
        .find(|(_, path, _, _)| path.ends_with("/comments"))
        .and_then(|(_, _, body, _)| body.as_ref()?.get("body")?.as_str())
        .unwrap_or("")
        .to_owned()
}

fn close_reason(gh: &Fake) -> String {
    gh.asked("PATCH")
        .into_iter()
        .last()
        .and_then(|(_, _, body, _)| body.as_ref()?.get("state_reason")?.as_str())
        .unwrap_or("")
        .to_owned()
}

#[test]
fn door_rejects_non_owner_door_author_edits_renames_deletions_and_direct_mutations() {
    let (dir, cfg, _) = rig();
    let owner_author = owner();
    let delegate = Author {
        login: "delegate".into(),
        id: 99,
    };
    let door_cfg = DoorConfig {
        repo: "sam/box".into(),
        label: "settings".into(),
        authors: vec![owner_author.clone(), delegate.clone()],
        state: dir.path().join("door"),
        settings: cfg.clone(),
    };

    let v1_bin = dir.path().join("third-v1");
    let v1_hash = write_plugin_fixture(
        &v1_bin,
        "third",
        r#"[{"id":"m1","efforts":["low"]}]"#,
        "none",
        true,
        None,
        None,
    );
    let v2_bin = dir.path().join("third-v2");
    let v2_hash = write_plugin_fixture(
        &v2_bin,
        "third",
        r#"[{"id":"m1","efforts":["low"]},{"id":"m2","efforts":["high"]}]"#,
        "none",
        true,
        None,
        None,
    );
    let install_v1 = format!(
        "plugin install third release {} 1.0.0 {v1_hash}",
        v1_bin.display()
    );
    let update_v2 = format!(
        "plugin update third release {} 2.0.0 {v2_hash}",
        v2_bin.display()
    );

    // 1. Delegate (in door.authors, outside settings.owners) edits owner's issue body.
    let mut gh = fake_door_gh(
        10,
        ("sam", owner_author.id),
        vec![issue_graphql(
            10,
            ("sam", owner_author.id),
            "settings",
            &install_v1,
            vec![json!({"editor": actor_json("delegate", delegate.id), "deletedBy": null})],
            vec![],
        )],
    );
    pass(&mut gh, &door_cfg).unwrap();
    assert!(cfg.installed_plugin("third").unwrap().is_none());
    assert!(
        posted_comment(&gh).contains("Nothing changed:"),
        "{}",
        posted_comment(&gh)
    );
    assert_eq!(close_reason(&gh), "not_planned");

    // 2. Delegate renames title of owner's blank-body issue to `plugin install ...`.
    let mut gh = fake_door_gh(
        11,
        ("sam", owner_author.id),
        vec![issue_graphql(
            11,
            ("sam", owner_author.id),
            &install_v1,
            "",
            vec![],
            vec![json!({"actor": actor_json("delegate", delegate.id)})],
        )],
    );
    pass(&mut gh, &door_cfg).unwrap();
    assert!(cfg.installed_plugin("third").unwrap().is_none());
    assert_eq!(close_reason(&gh), "not_planned");

    // 3. Delegate deletes an edit on owner's issue (`deletedBy: delegate`).
    let mut gh = fake_door_gh(
        12,
        ("sam", owner_author.id),
        vec![issue_graphql(
            12,
            ("sam", owner_author.id),
            "settings",
            &install_v1,
            vec![json!({
                "editor": actor_json("sam", owner_author.id),
                "deletedBy": actor_json("delegate", delegate.id)
            })],
            vec![],
        )],
    );
    pass(&mut gh, &door_cfg).unwrap();
    assert!(cfg.installed_plugin("third").unwrap().is_none());
    assert_eq!(close_reason(&gh), "not_planned");

    // 4. Delegate opens a plugin install issue directly.
    let mut gh = fake_door_gh(
        13,
        ("delegate", delegate.id),
        vec![issue_graphql(
            13,
            ("delegate", delegate.id),
            "settings",
            &install_v1,
            vec![],
            vec![],
        )],
    );
    pass(&mut gh, &door_cfg).unwrap();
    assert!(cfg.installed_plugin("third").unwrap().is_none());
    assert_eq!(close_reason(&gh), "not_planned");

    // No unauthorized plugin mutation may leave an audit entry in overlay.log.
    assert!(!cfg.overlay.with_extension("log").exists());

    // 5. Delegate can still ask read-only `plugin list`.
    let mut gh = fake_door_gh(
        14,
        ("delegate", delegate.id),
        vec![issue_graphql(
            14,
            ("delegate", delegate.id),
            "settings",
            "plugin list",
            vec![],
            vec![],
        )],
    );
    pass(&mut gh, &door_cfg).unwrap();
    assert!(
        posted_comment(&gh).contains("No installed engine plugins"),
        "{}",
        posted_comment(&gh)
    );
    assert_eq!(close_reason(&gh), "completed");

    // 6. TOCTOU: delegate edits between door snapshot and owner snapshot -> rejected.
    let mut gh = fake_door_gh(
        15,
        ("sam", owner_author.id),
        vec![
            issue_graphql(
                15,
                ("sam", owner_author.id),
                "settings",
                &install_v1,
                vec![],
                vec![],
            ),
            issue_graphql(
                15,
                ("sam", owner_author.id),
                "settings",
                &install_v1,
                vec![json!({"editor": actor_json("delegate", delegate.id), "deletedBy": null})],
                vec![],
            ),
        ],
    );
    pass(&mut gh, &door_cfg).unwrap();
    assert!(cfg.installed_plugin("third").unwrap().is_none());
    assert_eq!(close_reason(&gh), "not_planned");

    // 7. Authentic owner issue installs v1 cleanly (and parses from the owner-vetted snapshot).
    let install_v1_alt = format!(
        "plugin install third release {} 1.0.1 {v1_hash}",
        v1_bin.display()
    );
    let mut gh = fake_door_gh(
        16,
        ("sam", owner_author.id),
        vec![
            issue_graphql(
                16,
                ("sam", owner_author.id),
                "settings",
                &install_v1,
                vec![],
                vec![],
            ),
            issue_graphql(
                16,
                ("sam", owner_author.id),
                "settings",
                &install_v1_alt,
                vec![json!({"editor": actor_json("sam", owner_author.id), "deletedBy": null})],
                vec![],
            ),
        ],
    );
    pass(&mut gh, &door_cfg).unwrap();
    let installed = cfg.installed_plugin("third").unwrap().unwrap();
    assert_eq!(installed.version, "1.0.1");
    assert_eq!(close_reason(&gh), "completed");

    // 8. Delegate editing owner's issue to `plugin update` or `plugin remove` is rejected.
    for (num, cmd) in [
        (17, update_v2.as_str()),
        (18, "plugin remove third --fallback claude"),
    ] {
        let mut gh = fake_door_gh(
            num,
            ("sam", owner_author.id),
            vec![issue_graphql(
                num,
                ("sam", owner_author.id),
                "settings",
                cmd,
                vec![json!({"editor": actor_json("delegate", delegate.id), "deletedBy": null})],
                vec![],
            )],
        );
        pass(&mut gh, &door_cfg).unwrap();
        assert_eq!(
            cfg.installed_plugin("third").unwrap().unwrap().version,
            "1.0.1"
        );
        assert_eq!(close_reason(&gh), "not_planned");
    }

    // 9. Empty `settings.owners` fails closed even for a door author.
    let mut no_owners_cfg = door_cfg;
    no_owners_cfg.settings.owners.clear();
    let mut gh = fake_door_gh(
        19,
        ("sam", owner_author.id),
        vec![issue_graphql(
            19,
            ("sam", owner_author.id),
            "settings",
            "plugin remove third --fallback claude",
            vec![],
            vec![],
        )],
    );
    pass(&mut gh, &no_owners_cfg).unwrap();
    assert!(cfg.installed_plugin("third").unwrap().is_some());
    assert!(
        posted_comment(&gh).contains("no authorized owners"),
        "{}",
        posted_comment(&gh)
    );
    assert_eq!(close_reason(&gh), "not_planned");
}

#[test]
fn remove_plugin_succeeds_with_unrelated_stale_saved_setups() {
    let (dir, cfg, _) = rig();

    // Install two plugins: `second` and `third`.
    let second_v1 = dir.path().join("second-v1");
    let second_v1_hash = write_plugin_fixture(
        &second_v1,
        "second",
        r#"[{"id":"s-old","efforts":["low"]},{"id":"s-new","efforts":["high"]}]"#,
        "none",
        true,
        None,
        None,
    );
    let second_v2 = dir.path().join("second-v2");
    let second_v2_hash = write_plugin_fixture(
        &second_v2,
        "second",
        r#"[{"id":"s-new","efforts":["high"]}]"#,
        "none",
        true,
        None,
        None,
    );
    let third_bin = dir.path().join("third-v1");
    let third_hash = write_plugin_fixture(
        &third_bin,
        "third",
        r#"[{"id":"t1","efforts":["low"]}]"#,
        "none",
        true,
        None,
        None,
    );

    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install second release {} 1.0.0 {second_v1_hash}",
            second_v1.display()
        )),
    )
    .unwrap();
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin install third release {} 1.0.0 {third_hash}",
            third_bin.display()
        )),
    )
    .unwrap();

    // 1. Save an untouched setup naming `claude` model `old-m`, then withdraw `old-m` via `offer`.
    cfg.command(&words("offer claude models old-m"), "host")
        .unwrap();
    cfg.command(&words("model work claude old-m"), "host")
        .unwrap();
    cfg.command(&words("engines save stale-claude"), "host")
        .unwrap();
    cfg.command(&words("model work claude unset"), "host")
        .unwrap();
    cfg.command(&words("offer claude models new-m"), "host")
        .unwrap();

    // 2. Save a setup that combines a `second` model (`s-old`) and a `third` route + model,
    //    then withdraw `s-old` via `plugin update second`.
    cfg.command(&words("model work second s-old low"), "host")
        .unwrap();
    cfg.command(&words("role review third"), "host").unwrap();
    cfg.command(&words("model review third t1 low"), "host")
        .unwrap();
    cfg.command(&words("engines save combo"), "host").unwrap();
    cfg.command(&words("model work second unset"), "host")
        .unwrap();
    cfg.command_as(
        &owner_auth(),
        &words(&format!(
            "plugin update second release {} 2.0.0 {second_v2_hash}",
            second_v2.display()
        )),
    )
    .unwrap();

    // 3. Save a clean setup referencing `third` and activate it so `previous` also references `third`.
    cfg.command(&words("role fix third over 80 claude"), "host")
        .unwrap();
    cfg.command(&words("engines save clean-third"), "host")
        .unwrap();
    cfg.command(&words("role fix claude"), "host").unwrap();
    cfg.command(&words("engines use clean-third"), "host")
        .unwrap();

    // Removing `third` must NOT be blocked by `stale-claude` or `combo`'s stale `second` model.
    let out = cfg
        .command_as(
            &owner_auth(),
            &words("plugin remove third --fallback claude"),
        )
        .unwrap();
    assert!(out.contains("Removed engine plugin third 1.0.0"), "{out}");
    assert!(cfg.installed_plugin("third").unwrap().is_none());

    // Rewritten clean setup can be activated cleanly; stale setups still fail on `engines use`.
    cfg.command(&words("engines use clean-third"), "host")
        .unwrap();
    assert_eq!(cfg.roles().unwrap().route("fix", None).engine, "claude");
    assert!(
        cfg.command(&words("engines use stale-claude"), "host")
            .is_err()
    );
    assert!(cfg.command(&words("engines use combo"), "host").is_err());

    // Confirm `combo` had its `third` route rewritten to `claude` and `third` model dropped.
    let raw: Value = serde_json::from_str(&fs::read_to_string(cfg.roles_file()).unwrap()).unwrap();
    let combo = &raw["setups"]["saved"]["combo"];
    assert_eq!(combo["roles"]["review"]["engine"], json!("claude"));
    assert!(combo["models"].get("review third").is_none());
}

#[test]
fn plugin_list_and_verify_do_not_panic_on_short_or_multibyte_sha256() {
    let (dir, cfg, _) = rig();
    let bin = dir.path().join("third-v1");
    let hash = write_plugin_fixture(
        &bin,
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
            bin.display()
        )),
    )
    .unwrap();

    // Single-read `plugin verify third` succeeds.
    let verified = cfg.command(&words("plugin verify third"), "host").unwrap();
    assert!(
        verified.contains(&format!("third verified {hash}")),
        "{verified}"
    );

    // Hand-edited short or multi-byte `sha256` in `overlay.roles.json` must not panic `plugin list`.
    for bad_sha in ["sha256:abcd", "sha256:ééééééé"] {
        let mut raw: Value =
            serde_json::from_str(&fs::read_to_string(cfg.roles_file()).unwrap()).unwrap();
        raw["plugins"]["third"]["sha256"] = json!(bad_sha);
        fs::write(
            cfg.roles_file(),
            serde_json::to_string_pretty(&raw).unwrap(),
        )
        .unwrap();

        let listed = cfg.command(&words("plugin list"), "host").unwrap();
        assert!(listed.contains(bad_sha), "{bad_sha} -> {listed}");
    }
}
