use super::*;
use burrow_producer::jobs::{chosen, offered, runnable};

/// A machine: its settings source (a file, as the build writes it) and a
/// base config for one repo, with the proof plugin's own choice in it.
struct Rig {
    _dir: tempfile::TempDir,
    settings: burrow_settings::Config,
    manifest: Manifest,
}

const BASE: &str = r#"{"plugins":{"fix":"/x/fix","judge":"/x/judge","proof":"/x/proof"},"execution":{"proof":{"engine":"burrow-proof","model":null,"effort":null}},"trusted_authors":["sam"]}"#;

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let settings = burrow_settings::Config {
        overlay: dir.path().join("settings/overlay.json"),
        ..Default::default()
    };
    fs::create_dir_all(dir.path().join("settings")).unwrap();
    let file = dir.path().join("settings.json");
    fs::write(&file, serde_json::to_vec(&settings).unwrap()).unwrap();
    let base = dir.path().join("base");
    fs::create_dir_all(base.join("sam")).unwrap();
    fs::write(base.join("sam/homelab.json"), BASE).unwrap();
    let manifest = Manifest {
        settings: file,
        base,
        out: dir.path().join("out"),
        roles: BTreeMap::from([
            ("fix".into(), "fix".into()),
            ("judge".into(), "judge".into()),
        ]),
    };
    Rig {
        _dir: dir,
        settings,
        manifest,
    }
}

impl Rig {
    fn set(&self, line: &str) {
        let args: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        self.settings.command(&args, "test").unwrap();
    }
    fn out(&self) -> PathBuf {
        self.manifest.out.join("sam/homelab.json")
    }
    fn rendered(&self) -> Config {
        conary_github::flow::schema::load::<Config>(&fs::read(self.out()).unwrap())
            .unwrap()
            .0
    }
    fn execution(&self, kind: &str) -> Choice {
        let kind = serde_json::from_value(serde_json::json!(kind)).unwrap();
        self.rendered().execution[&kind].clone()
    }
}

#[test]
fn a_choice_with_no_live_model_is_refused_by_every_job() {
    // What a build rendered (burrow#1393): the declared defaults, no model,
    // which the jobs then refuse.
    let r = rig();
    let built = jobs::choice(&r.settings, "judge").unwrap();
    assert_eq!(built.model, None);
    assert!(chosen(Some(&built)).unwrap_err().contains("chose no model"));
    // And the render refuses it in turn, writing nothing.
    let e = render(&r.manifest).unwrap_err();
    assert!(e.contains("names no model"), "{e}");
    assert!(!r.out().exists());
}

#[test]
fn the_live_role_settings_are_the_rendered_choices() {
    let r = rig();
    r.set("offer claude models stand-in-one stand-in-two");
    r.set("model fix claude stand-in-one high");
    r.set("model judge claude stand-in-two");
    assert_eq!(render(&r.manifest).unwrap(), 1);

    let fix = r.execution("fix");
    assert_eq!(
        (
            fix.engine.as_str(),
            fix.model.as_deref(),
            fix.effort.as_deref()
        ),
        ("claude", Some("stand-in-one"), Some("high"))
    );
    let judge = r.execution("judge");
    assert_eq!(judge.model.as_deref(), Some("stand-in-two"));
    assert_eq!(judge.effort, None);
    // The base's own stays: the proof plugin's, which offers no model.
    let proof = r.execution("proof");
    assert_eq!((proof.engine.as_str(), proof.model), ("burrow-proof", None));
    // Only the execution is added: the rest is the base's.
    assert_eq!(r.rendered().trusted_authors, ["sam"]);

    // What a job is held to accepts it.
    for (kind, role) in [("fix", "fix"), ("judge", "judge")] {
        let choice = r.execution(kind);
        chosen(Some(&choice)).unwrap();
        let offer = offered(&r.settings, role).unwrap();
        runnable(&r.settings, role, "sam/homelab", &offer, Some(&choice)).unwrap();
    }
}

#[test]
fn a_change_to_the_roles_is_read_on_the_next_render() {
    let r = rig();
    r.set("offer claude models stand-in-one stand-in-two");
    r.set("model fix claude stand-in-one");
    r.set("model judge claude stand-in-one");
    render(&r.manifest).unwrap();
    assert_eq!(r.execution("fix").model.as_deref(), Some("stand-in-one"));

    r.set("model fix claude stand-in-two low");
    render(&r.manifest).unwrap();
    let fix = r.execution("fix");
    assert_eq!(
        (fix.model.as_deref(), fix.effort.as_deref()),
        (Some("stand-in-two"), Some("low"))
    );
    // The choice a job holds a request to is the new one, and the old one
    // is refused.
    let offer = offered(&r.settings, "fix").unwrap();
    runnable(&r.settings, "fix", "sam/homelab", &offer, Some(&fix)).unwrap();
    let old = Choice {
        model: Some("stand-in-one".into()),
        effort: None,
        ..fix
    };
    assert!(runnable(&r.settings, "fix", "sam/homelab", &offer, Some(&old)).is_err());
}

#[test]
fn a_refused_choice_leaves_the_last_render_as_it_was() {
    let r = rig();
    r.set("offer claude models stand-in-one");
    r.set("model fix claude stand-in-one");
    r.set("model judge claude stand-in-one");
    render(&r.manifest).unwrap();
    let before = fs::read(r.out()).unwrap();

    // The engine stops offering the model the roles are set to: by a hand
    // edit, since the settings door refuses an offer that strands a role.
    let roles = r.settings.roles_file();
    let mut doc: serde_json::Value = serde_json::from_slice(&fs::read(&roles).unwrap()).unwrap();
    doc["offers"]["claude"]["models"] = serde_json::json!(["stand-in-two"]);
    fs::write(&roles, serde_json::to_vec(&doc).unwrap()).unwrap();
    let e = render(&r.manifest).unwrap_err();
    assert!(e.contains("doesn't offer that model"), "{e}");
    assert_eq!(fs::read(r.out()).unwrap(), before);
}

#[test]
fn a_role_with_no_model_on_an_engine_that_offers_some_is_refused() {
    let r = rig();
    r.set("offer claude models stand-in-one");
    r.set("model judge claude stand-in-one");
    let e = render(&r.manifest).unwrap_err();
    assert!(
        e.starts_with("fix: ") && e.contains("names no model of claude's"),
        "{e}"
    );
    assert!(!r.out().exists());
}

#[test]
fn a_repo_no_longer_in_the_base_has_no_config() {
    let mut r = rig();
    r.manifest.roles.clear();
    fs::create_dir_all(r.manifest.base.join("other")).unwrap();
    fs::write(r.manifest.base.join("other/box.json"), "{}").unwrap();
    assert_eq!(render(&r.manifest).unwrap(), 2);
    // With no role-run plugin, a base is rendered as it was.
    let as_built: serde_json::Value = serde_json::from_str(BASE).unwrap();
    let rendered: serde_json::Value = serde_json::from_slice(&fs::read(r.out()).unwrap()).unwrap();
    assert_eq!(rendered, as_built);

    fs::remove_dir_all(r.manifest.base.join("other")).unwrap();
    assert_eq!(render(&r.manifest).unwrap(), 1);
    assert!(!r.manifest.out.join("other").exists());
    assert!(r.out().exists());
}

#[test]
fn a_config_conary_refuses_isnt_written() {
    let r = rig();
    r.set("offer claude models stand-in-one");
    r.set("model fix claude stand-in-one");
    r.set("model judge claude stand-in-one");
    fs::write(
        r.manifest.base.join("sam/homelab.json"),
        r#"{"no_such_option":1}"#,
    )
    .unwrap();
    assert!(render(&r.manifest).is_err());
    assert!(!r.out().exists());
}
