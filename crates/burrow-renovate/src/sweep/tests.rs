use super::*;

#[test]
fn its_config_is_every_setting_and_only_those() {
    let config = serde_json::json!({
        "name": "renovate-sweep", "state": "/s/renovate-sweep", "projects": "/p", "owner": "Sam",
        "settled": "review-settled", "covered": "/s/autoland/repos.json",
        "app": {"name": "workers", "bot": "sam-workers"}, "renovate": "sam-renovate[bot]", "renovateRun": 7200,
        "tools": {"gh": "/b/gh", "git": "/b/git", "proj": "/b/proj", "notify": "/b/notify",
                  "seal": "/b/major-seal", "lockWait": "/b/lock-wait"},
    });
    let cfg: Config = serde_json::from_value(config.clone()).unwrap();
    assert_eq!(cfg.app.unwrap().bot, "sam-workers");
    assert_eq!(cfg.renovate.as_deref(), Some("sam-renovate[bot]"));
    assert_eq!(cfg.renovate_run, 7200);
    let mut user = config.clone();
    user["app"] = Value::Null;
    user["covered"] = Value::Null;
    user["renovate"] = Value::Null;
    let cfg: Config = serde_json::from_value(user).unwrap();
    assert!(cfg.app.is_none() && cfg.covered.is_none() && cfg.renovate.is_none());
    let mut more = config.clone();
    more["stray"] = 1.into();
    assert!(serde_json::from_value::<Config>(more).is_err());
    let mut less = config;
    less["tools"].as_object_mut().unwrap().remove("lockWait");
    assert!(serde_json::from_value::<Config>(less).is_err());
}

#[test]
fn projects_are_as_a_shell_globs_them_the_hidden_ones_last() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["b", "a", ".github", ".a", "B"] {
        fs::create_dir(dir.path().join(name)).unwrap();
    }
    fs::write(dir.path().join("file"), "").unwrap();
    std::os::unix::fs::symlink(dir.path().join("a"), dir.path().join("c")).unwrap();
    assert_eq!(projects(dir.path()), ["B", "a", "b", "c", ".a", ".github"]);
    assert!(projects(&dir.path().join("none")).is_empty());
}

#[test]
fn a_request_taken_back_is_its_head_tries_and_when() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sam_r-1.released");
    assert_eq!(taken_back(&path), (String::new(), 0, 0));
    fs::write(&path, "abc 2 1000\n").unwrap();
    assert_eq!(taken_back(&path), ("abc".into(), 2, 1000));
    assert_eq!(
        with_suffix(&dir.path().join("k"), ".released"),
        dir.path().join("k.released")
    );
}
