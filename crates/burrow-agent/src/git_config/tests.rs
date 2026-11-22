use super::*;

#[test]
fn no_login_survives_in_a_kept_value() {
    for (key, value, kept) in [
        (
            "remote.origin.url",
            "https://user:secret@example.com/o/r",
            "https://example.com/o/r",
        ),
        (
            "remote.origin.url",
            "https://secret@example.com/o/r.git?token=secret",
            "https://example.com/o/r.git",
        ),
        (
            "remote.origin.url",
            "https://example.com/o/r",
            "https://example.com/o/r",
        ),
        (
            "remote.origin.url",
            "git@example.com:o/r",
            "example.com:o/r",
        ),
        (
            "remote.origin.url",
            "user:secret@example.com:o/r",
            "user:example.com:o/r",
        ),
        ("remote.origin.url", "/srv/git/r", "/srv/git/r"),
        (
            "submodule.vendor/x.url",
            "https://secret@example.com/o/x",
            "https://example.com/o/x",
        ),
        ("branch.main.remote", "origin", "origin"),
        (
            "branch.main.remote",
            "https://secret@example.com/o/r",
            "https://example.com/o/r",
        ),
        ("core.bare", "false", "false"),
    ] {
        assert_eq!(keep(key, value).as_deref(), Some(kept), "{key} {value}");
    }
    for key in [
        "http.https://example.com/.extraheader",
        "http.extraheader",
        "credential.helper",
        "url.https://secret@example.com/.insteadof",
        "include.path",
        "alias.x",
        "core.sshcommand",
        "core.hookspath",
        "remote.origin.pushurl",
        "user.name",
        "remote.https://secret@example.com/o/r.fetch",
    ] {
        assert_eq!(keep(key, "secret"), None, "{key}");
    }
}

#[test]
fn every_config_of_the_repository_is_rewritten() {
    let dir = tempfile::tempdir().unwrap();
    let git = dir.path().join("git");
    let configs = [
        "config",
        "config.worktree",
        "modules/vendor/x/config",
        "worktrees/w/config.worktree",
    ];
    for config in configs {
        let at = git.join(config);
        fs::create_dir_all(at.parent().unwrap()).unwrap();
        fs::write(
            at,
            "[core]\n\tbare = false\n[http]\n\textraheader = AUTHORIZATION: secret\n\
             [remote \"origin\"]\n\turl = https://secret@example.com/o/r\n",
        )
        .unwrap();
    }
    // A branch by that name is no config.
    fs::create_dir_all(git.join("refs/heads")).unwrap();
    fs::write(git.join("refs/heads/config"), "0000\n").unwrap();
    let mut cfg = crate::new::tests::config(dir.path());
    cfg.path = std::env::var("PATH").unwrap();
    let agent = Agent::new(&cfg, Path::new("/nonexistent.json")).unwrap();
    sanitize(&agent, &git).unwrap();
    for config in configs {
        let text = fs::read_to_string(git.join(config)).unwrap();
        assert!(!text.contains("secret"), "{config}: {text}");
        assert!(
            text.contains("url = https://example.com/o/r"),
            "{config}: {text}"
        );
    }
    assert_eq!(
        fs::read_to_string(git.join("refs/heads/config")).unwrap(),
        "0000\n"
    );
}
