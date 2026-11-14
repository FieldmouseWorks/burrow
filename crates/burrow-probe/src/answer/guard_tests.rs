//! What the probe never reads, and what it says when it can't tell: the
//! reviewers' matching rules, links spelled around a hidden name, a
//! directory held open, a PATH the probe couldn't search whole, and the
//! environment `runners` reads facts in.

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use super::facts_tests::{Fake, HASH, base, command, config, machine, show};
use super::shown::Unread;
use crate::Config;
use crate::query::Query;

fn exe(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    burrow_test::executable(path, "#!/bin/sh\n");
}

fn runner(cfg: &Config, root: &Path, fake: &Fake) -> String {
    machine(cfg, root, fake)
        .answer(&Query::Runners {
            name: Some("ci-runner".into()),
        })
        .unwrap()
}

#[test]
fn a_directory_pattern_hides_the_directory_and_what_is_in_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("home/ci/private")).unwrap();
    fs::write(root.join("home/ci/private/settings.toml"), "x = 1\n").unwrap();
    fs::write(root.join("home/ci/open.toml"), "x = 1\n").unwrap();
    let mut cfg = config(root);
    cfg.hidden = vec!["^private/$".into()];
    let fake = Fake::default();
    let m = machine(&cfg, root, &fake);
    assert_eq!(m.read_small("/home/ci/open.toml", 100).unwrap(), "x = 1\n");
    for path in ["/home/ci/private", "/home/ci/private/settings.toml"] {
        assert_eq!(m.resolve(path), Err(Unread::Hidden), "{path}");
        assert_eq!(m.read_small(path, 100), Err(Unread::Hidden), "{path}");
    }
    assert_eq!(m.list("/home/ci/private").unwrap_err(), Unread::Hidden);
    // And in a listing, by its name.
    exe(&root.join("home/ci/bin/tool"));
    fs::create_dir_all(root.join("home/ci/bin/private")).unwrap();
    let listing = m.list("/home/ci/bin").unwrap();
    assert_eq!((listing.entries.len(), listing.hidden), (1, 1));
}

#[test]
fn a_name_ending_in_a_newline_is_held_to_the_patterns_without_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("opt/bin")).unwrap();
    let odd = root.join("opt/bin/credential.key\n");
    exe(&odd);
    exe(&root.join("opt/bin/tool"));
    symlink("credential.key\n", root.join("opt/bin/alias")).unwrap();
    let cfg = config(root);
    let fake = Fake::default();
    let m = machine(&cfg, root, &fake);
    assert_eq!(
        m.read_small("/opt/bin/credential.key\n", 100),
        Err(Unread::Hidden)
    );
    // Direct, in the listing, as a command, and as a link's target.
    let listing = m.list("/opt/bin").unwrap();
    assert_eq!(listing.entries.len(), 1, "{listing:?}");
    assert_eq!(listing.hidden, 2, "{listing:?}");
    assert_eq!(
        m.entry("/opt/bin", "credential.key\n").unwrap_err(),
        Unread::Hidden
    );
    assert_eq!(m.entry("/opt/bin", "alias").unwrap_err(), Unread::Hidden);
}

#[test]
fn a_link_cannot_spell_a_hidden_path_away() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("srv/rustup")).unwrap();
    fs::write(
        root.join("srv/rustup/settings.toml"),
        "default_toolchain = \"x\"\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("home/ci")).unwrap();
    let mut cfg = config(root);
    cfg.hidden = vec!["^private/settings\\.toml$".into()];
    let fake = Fake::default();
    let m = machine(&cfg, root, &fake);
    // Absolute and relative links to a directory, then the hidden name
    // below: neither the link's name nor its target's names match.
    symlink("/srv/rustup", root.join("home/ci/private")).unwrap();
    symlink("../../srv/rustup", root.join("home/ci/rel")).unwrap();
    assert_eq!(
        m.read_small("/home/ci/private/settings.toml", 100),
        Err(Unread::Hidden)
    );
    // Through a second link in the chain too.
    symlink("private", root.join("home/ci/again")).unwrap();
    assert_eq!(
        m.read_small("/home/ci/again/settings.toml", 100),
        Err(Unread::Hidden)
    );
    cfg.hidden = vec!["^rel/settings\\.toml$".into()];
    let m = machine(&cfg, root, &fake);
    assert_eq!(
        m.read_small("/home/ci/rel/settings.toml", 100),
        Err(Unread::Hidden)
    );
    // Not hidden, not refused.
    assert!(m.read_small("/home/ci/private/settings.toml", 100).is_ok());
}

#[test]
fn a_checked_directory_stays_the_one_that_was_checked() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fs::create_dir_all(root.join("home/ci/.rustup")).unwrap();
    fs::write(root.join("home/ci/.rustup/settings.toml"), "open\n").unwrap();
    fs::create_dir_all(root.join("run/secrets")).unwrap();
    fs::write(root.join("run/secrets/settings.toml"), "SECRET\n").unwrap();
    let cfg = config(root);
    let fake = Fake::default();
    let m = machine(&cfg, root, &fake);
    let g = m.guard().unwrap();
    let held = m
        .resolve_with(&g, "/home/ci/.rustup/settings.toml")
        .unwrap();
    // The runner swaps the checked directory for a link to a hidden one.
    fs::rename(root.join("home/ci/.rustup"), root.join("home/ci/old")).unwrap();
    symlink("/run/secrets", root.join("home/ci/.rustup")).unwrap();
    let fd = rustix::fs::openat(
        &held.dir,
        held.leaf.as_deref().unwrap(),
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    let mut text = String::new();
    std::io::Read::read_to_string(&mut fs::File::from(fd), &mut text).unwrap();
    assert_eq!(text, "open\n");
    // And a read begun afterwards meets the link and the hidden path.
    assert_eq!(
        m.read_small("/home/ci/.rustup/settings.toml", 100),
        Err(Unread::Hidden)
    );
}

#[test]
fn a_link_that_leads_nowhere_or_to_a_non_command_is_not_a_command() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    base(root);
    command(root, "/run/current-system/sw/bin", "rustup-1.28.2", "rustc");
    let bin = root.join("etc/actions-runner/ci-runner/bin");
    fs::create_dir_all(&bin).unwrap();
    symlink("/missing", bin.join("rustc")).unwrap();
    // A link to a directory, a file that doesn't run, and a directory.
    fs::create_dir_all(root.join("opt/dir")).unwrap();
    symlink("/opt/dir", bin.join("cargo")).unwrap();
    fs::write(root.join("opt/data"), "x").unwrap();
    symlink("/opt/data", bin.join("rustup")).unwrap();
    fs::create_dir_all(bin.join("subdir")).unwrap();
    let cfg = config(root);
    let fake = Fake::default();
    let got = runner(&cfg, root, &fake);
    assert!(
        got.contains("rustc in /run/current-system/sw/bin (rustup-1.28.2)"),
        "{got}"
    );
    assert!(
        got.contains("cargo not on PATH, rustup not on PATH"),
        "{got}"
    );
    // Counted: git and node of the base, not the four others.
    assert!(
        got.contains("/etc/actions-runner/ci-runner/bin: 2 commands from"),
        "{got}"
    );
    // The package is the file's, however many links lead to it.
    let real = root.join(format!("nix/store/{HASH}-real-1.0/bin"));
    fs::create_dir_all(&real).unwrap();
    exe(&real.join("tool"));
    symlink(
        format!("/nix/store/{HASH}-real-1.0/bin/tool"),
        root.join("opt/hop"),
    )
    .unwrap();
    symlink("/opt/hop", bin.join("tool")).unwrap();
    let got = runner(&cfg, root, &fake);
    assert!(got.contains("real-1.0"), "{got}");
}

#[test]
fn a_path_directory_the_probe_could_not_search_leaves_the_toolchain_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    base(root);
    // A hidden directory first on PATH, rustc in a later one.
    command(root, "/run/secrets/bin", "rustc-0", "rustc");
    command(root, "/run/current-system/sw/bin", "rustup-1.28.2", "rustc");
    let mut cfg = config(root);
    cfg.runners[0].environment.insert(
        "PATH".into(),
        "/run/secrets/bin:/run/current-system/sw/bin".into(),
    );
    let fake = Fake::default();
    let got = runner(&cfg, root, &fake);
    assert!(
        got.contains("rustc unknown: in /run/current-system/sw/bin (rustup-1.28.2), unless an earlier PATH directory holds one, which the probe could not search: /run/secrets/bin"),
        "{got}"
    );
    assert!(
        got.contains("default toolchain: unknown: a toolchain command on PATH isn't read"),
        "{got}"
    );
    assert!(!got.contains("neither rustc nor rustup"), "{got}");

    // Nowhere found, and a directory unsearched: not "not on PATH".
    cfg.runners[0].environment.insert(
        "PATH".into(),
        "/run/secrets/bin:/etc/actions-runner/ci-runner/bin".into(),
    );
    let got = runner(&cfg, root, &fake);
    assert!(
        got.contains("cargo unknown: not found in the directories searched; the probe could not search /run/secrets/bin"),
        "{got}"
    );
    assert!(!got.contains("neither rustc nor rustup"), "{got}");

    // Relative and empty entries depend on a working directory.
    cfg.runners[0].environment.insert(
        "PATH".into(),
        ":bin:/etc/actions-runner/ci-runner/bin".into(),
    );
    let got = runner(&cfg, root, &fake);
    assert!(got.contains("an empty entry (not absolute"), "{got}");
    assert!(got.contains("bin (not absolute"), "{got}");
    assert!(
        got.contains("cargo unknown: not found in the directories searched; the probe could not search an empty entry, bin"),
        "{got}"
    );

    // A directory the probe may not read, as a user.
    if !fs::read_to_string("/proc/self/status")
        .unwrap()
        .contains("Uid:\t0\t")
    {
        let locked = root.join("opt/locked");
        fs::create_dir_all(&locked).unwrap();
        burrow_test::permissions(&locked, 0o000);
        cfg.runners[0].environment.insert(
            "PATH".into(),
            "/opt/locked:/etc/actions-runner/ci-runner/bin".into(),
        );
        let got = runner(&cfg, root, &fake);
        burrow_test::permissions(&locked, 0o755);
        assert!(
            got.contains("cargo unknown: not found in the directories searched; the probe could not search /opt/locked (unknown: the probe may not read it)"),
            "{got}"
        );
    }
}

fn toolchain_world(root: &Path) {
    base(root);
    command(
        root,
        "/etc/actions-runner/ci-runner/bin",
        "rustup-1.28.2",
        "rustup",
    );
    let rustup = root.join("var/lib/ci/.rustup");
    fs::create_dir_all(&rustup).unwrap();
    fs::write(
        rustup.join("settings.toml"),
        "default_toolchain = \"stable-x86_64-unknown-linux-gnu\"\n",
    )
    .unwrap();
}

fn loaded(user: &str, env: &str) -> Fake {
    Fake {
        show: format!(
            "Id=ci-runner.service\nLoadState=loaded\nActiveState=active\nSubState=running\nUser={user}\nDynamicUser=no\nNeedDaemonReload=no\nEnvironment=PATH=/etc/actions-runner/ci-runner/bin {env}\n"
        ),
        ..Fake::default()
    }
}

#[test]
fn found_facts_are_read_in_the_environment_systemd_loaded() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    toolchain_world(root);
    let mut cfg = config(root);
    // Added since the unit loaded: HOME and CARGO_HOME are not restored.
    cfg.runners[0]
        .environment
        .insert("RUSTUP_HOME".into(), "/var/lib/ci/elsewhere".into());
    let got = runner(&cfg, root, &loaded("ci", "HOME=/var/lib/ci"));
    assert!(
        got.contains(
            "default toolchain: stable-x86_64-unknown-linux-gnu (/var/lib/ci/.rustup/settings.toml)"
        ),
        "{got}"
    );
    assert!(
        got.contains("loaded: no RUSTUP_HOME, though one is configured"),
        "{got}"
    );
    assert!(
        got.contains("found: read in the environment loaded for ci-runner.service"),
        "{got}"
    );
    // The loaded one has what the configuration lacks.
    let cfg = config(root);
    let got = runner(&cfg, root, &loaded("ci", "RUSTUP_TOOLCHAIN=1.80.0"));
    assert!(
        got.contains("default toolchain: 1.80.0 (RUSTUP_TOOLCHAIN in its environment)"),
        "{got}"
    );
    // HOME removed, same user: the user's own home still stands.
    let got = runner(&cfg, root, &loaded("ci", ""));
    assert!(got.contains("(/var/lib/ci/.rustup/settings.toml)"), "{got}");
    // A user changed: the configured user's home isn't theirs.
    let got = runner(&cfg, root, &loaded("other", ""));
    assert!(
        got.contains("default toolchain: unknown: neither RUSTUP_HOME nor HOME is known"),
        "{got}"
    );
    // A PATH the loaded unit lacks is systemd's, not the configuration's.
    let mut bare = loaded("ci", "HOME=/var/lib/ci");
    bare.show = bare
        .show
        .replace("PATH=/etc/actions-runner/ci-runner/bin ", "");
    let got = runner(&cfg, root, &bare);
    assert!(got.contains("rustup not on PATH"), "{got}");
}

#[test]
fn fresh_instances_beyond_the_bound_are_counted_and_the_one_read_is_named() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    base(root);
    let cfg = config(root);
    let list: String = (0..5)
        .map(|i| format!("burrow-fresh-runner@{i}.service loaded active running x\n"))
        .collect();
    let fake = Fake {
        list,
        show: show("/run/current-system/sw/bin")
            .replace("ci-runner.service", "burrow-fresh-runner@0.service"),
        ..Fake::default()
    };
    let got = machine(&cfg, root, &fake)
        .answer(&Query::Runners { name: None })
        .unwrap();
    assert!(
        got.contains("loaded: 2 more instances not inspected (at most 3 are read); the found facts below describe burrow-fresh-runner@0.service only"),
        "{got}"
    );
    assert!(
        got.contains("found: read in the environment loaded for burrow-fresh-runner@0.service"),
        "{got}"
    );
    // Within the bound: nothing omitted.
    let few = Fake {
        list: "burrow-fresh-runner@0.service loaded active running x\n".into(),
        show: fake.show.clone(),
        ..Fake::default()
    };
    let got = machine(&cfg, root, &few)
        .answer(&Query::Runners { name: None })
        .unwrap();
    assert!(!got.contains("not inspected"), "{got}");
}

#[test]
fn github_errors_are_scrubbed_whole_before_they_are_cut() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let cfg = config(root);
    let token = format!("gh{}_{}", "p", "a1B2c3D4e5".repeat(4));
    let begin = format!("-----BEGIN RSA {} KEY-----", "PRIVATE");
    let end = format!("-----END RSA {} KEY-----", "PRIVATE");
    let secrets = [
        // Cut inside the token, its tail left too short to be one.
        (token.clone(), format!("{}{token} after", "x".repeat(490))),
        // Cut before the `@` that makes the userinfo a credential.
        (
            "hunter2hunter2".to_string(),
            format!(
                "{}https://ci:hunter2hunter2@example.com/x",
                " ".repeat(480).replace(' ', "y")
            ),
        ),
        // Cut inside a private key block.
        (
            "MIIEvQIBADANBgkqhkiG".to_string(),
            format!(
                "{}{begin}\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n{end}",
                "z".repeat(480)
            ),
        ),
    ];
    for (secret, said) in secrets {
        let (got, _) = ask_gh_error(&cfg, root, &said);
        let got = got.unwrap_err();
        assert!(!got.contains(&secret[..secret.len().min(12)]), "{got}");
        assert!(!got.contains("hunter2"), "{got}");
        assert!(got.starts_with("GitHub didn't say: "), "{got}");
        assert!(got.chars().count() <= 19 + 500, "{got}");
    }
}

fn ask_gh_error(cfg: &Config, root: &Path, said: &str) -> (Result<String, String>, Fake) {
    let fake = Fake::default();
    *fake.github.borrow_mut() = Some(Err(said.to_owned()));
    let got = machine(cfg, root, &fake).answer(&Query::Issue {
        repo: "sam/box".into(),
        number: 210,
    });
    (got, fake)
}

/// The probe holds paths to the steward's own rule, not a copy that drifts:
/// whatever the snapshot hides, the probe hides.
#[test]
fn the_probe_hides_whatever_the_snapshot_hides() {
    let patterns = [
        "(^|/)secrets?/",
        "\\.(pem|key|p12|pfx)$",
        "^private/$",
        "^vault/.+\\.toml$",
        "(^|/)roms/",
    ];
    let snapshot: burrow_upkeep::config::Snapshot = serde_json::from_value(serde_json::json!({
        "dir": "/x", "skipPaths": patterns, "repos": {}
    }))
    .unwrap();
    let hide = burrow_upkeep::snapshot::Hide::new(&snapshot, "sam/box").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.hidden = patterns.iter().map(|p| p.to_string()).collect();
    let fake = Fake::default();
    let m = machine(&cfg, dir.path(), &fake);
    let g = m.guard().unwrap();
    for path in [
        "a.txt",
        "key.pem",
        "key.pem\n",
        "credential.key\n",
        "a/secret/b",
        "secrets/x.yaml",
        "private/settings.toml",
        "private",
        "a/private/x",
        "vault/a.toml",
        "vault/a/b.toml",
        "a/roms/b/c",
        "a/romsx/c",
        "vault",
    ] {
        if hide.hidden(path.as_bytes()) {
            assert!(g.hidden(path, false), "{path:?}");
            assert!(g.hidden(&format!("/home/ci/{path}"), false), "{path:?}");
        }
    }
    // As a directory, a trailing-slash pattern takes the directory itself.
    assert!(g.hidden("/home/ci/private", true) && !g.hidden("/home/ci/private", false));
}

#[test]
fn entries_are_held_to_the_directory_name_they_were_asked_under() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    exe(&root.join("srv/rustup/rustc"));
    exe(&root.join("srv/rustup/cargo"));
    fs::create_dir_all(root.join("home/ci")).unwrap();
    symlink("/srv/rustup", root.join("home/ci/private")).unwrap();
    symlink("../../srv/rustup", root.join("home/ci/rel")).unwrap();
    symlink("private", root.join("home/ci/again")).unwrap();
    let mut cfg = config(root);
    cfg.hidden = vec!["^(private|rel|again)/rustc$".into()];
    let fake = Fake::default();
    let m = machine(&cfg, root, &fake);
    for dir in ["/home/ci/private", "/home/ci/rel", "/home/ci/again"] {
        assert_eq!(m.entry(dir, "rustc").unwrap_err(), Unread::Hidden, "{dir}");
        assert!(m.entry(dir, "cargo").unwrap().is_some(), "{dir}");
        let listing = m.list(dir).unwrap();
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!((names, listing.hidden), (vec!["cargo"], 1), "{dir}");
    }
    // Under its real name it is not hidden.
    assert!(m.entry("/srv/rustup", "rustc").unwrap().is_some());
}

#[test]
fn a_configured_home_is_not_restored_to_a_unit_loaded_without_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    toolchain_world(root);
    let new = root.join("opt/new-home/.rustup");
    fs::create_dir_all(&new).unwrap();
    fs::write(
        new.join("settings.toml"),
        "default_toolchain = \"from-new-home\"\n",
    )
    .unwrap();
    let mut cfg = config(root);
    cfg.runners[0]
        .environment
        .insert("HOME".into(), "/opt/new-home".into());
    // Loaded without HOME, the user's own home stands: not the new one.
    let got = runner(&cfg, root, &loaded("ci", ""));
    assert!(!got.contains("from-new-home"), "{got}");
    assert!(got.contains("/var/lib/ci/.rustup/settings.toml"), "{got}");
    // Nothing loaded: the configuration's.
    let got = runner(&cfg, root, &Fake::default());
    assert!(
        got.contains("default toolchain: from-new-home (/opt/new-home/.rustup/settings.toml)"),
        "{got}"
    );
}

#[test]
fn a_visible_rustup_does_not_make_an_unsearched_compiler_known() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    toolchain_world(root);
    command(root, "/run/secrets/bin", "rustc-0", "rustc");
    let mut cfg = config(root);
    for path in [
        "/etc/actions-runner/ci-runner/bin:/run/secrets/bin",
        "/etc/actions-runner/ci-runner/bin:",
        "/etc/actions-runner/ci-runner/bin:bin",
    ] {
        cfg.runners[0]
            .environment
            .insert("PATH".into(), path.into());
        let got = runner(&cfg, root, &Fake::default());
        assert!(
            got.contains("default toolchain: unknown: a toolchain command on PATH isn't read; rustup's own answer: stable-x86_64-unknown-linux-gnu"),
            "{path}: {got}"
        );
    }
    // The same with the toolchain chosen by the environment.
    cfg.runners[0]
        .environment
        .insert("RUSTUP_TOOLCHAIN".into(), "1.91.0".into());
    let got = runner(&cfg, root, &Fake::default());
    assert!(got.contains("default toolchain: unknown: a toolchain command on PATH isn't read; rustup's own answer: 1.91.0"), "{got}");
}

#[test]
fn a_command_is_held_to_the_name_of_the_link_its_directory_was_reached_by() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    exe(&root.join("srv/rustup/rustc"));
    exe(&root.join("srv/rustup/other"));
    fs::create_dir_all(root.join("home/ci")).unwrap();
    symlink("/srv/rustup", root.join("home/ci/private")).unwrap();
    symlink("private", root.join("home/ci/again")).unwrap();
    symlink("../../srv/rustup", root.join("home/ci/rel")).unwrap();
    let mut cfg = config(root);
    cfg.hidden = vec!["^private/rustc$".into(), "^again/other$".into()];
    let fake = Fake::default();
    let m = machine(&cfg, root, &fake);
    assert_eq!(
        m.entry("/home/ci/private", "rustc").unwrap_err(),
        Unread::Hidden
    );
    // A chained link, and a relative one that isn't named by a pattern.
    assert_eq!(
        m.entry("/home/ci/again", "other").unwrap_err(),
        Unread::Hidden
    );
    assert!(m.entry("/home/ci/rel", "rustc").unwrap().is_some());
    let listing = m.list("/home/ci/private").unwrap();
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["other"],
        "{listing:?}"
    );
    assert_eq!(listing.hidden, 1, "{listing:?}");
    let listing = m.list("/home/ci/again").unwrap();
    // Through the chain, `again/other` is hidden by its name, and
    // `rustc` by the `private/rustc` the chain's inner link makes.
    assert_eq!(
        (listing.entries.len(), listing.hidden),
        (0, 2),
        "{listing:?}"
    );
}

#[test]
fn dot_dot_after_a_link_leads_where_the_system_takes_it() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    // /opt/link -> /srv/sub, so /opt/link/.. is /srv, not /opt.
    exe(&root.join("srv/rustc"));
    exe(&root.join("opt/tool"));
    fs::create_dir_all(root.join("srv/sub")).unwrap();
    symlink("/srv/sub", root.join("opt/link")).unwrap();
    let cfg = config(root);
    let fake = Fake::default();
    let m = machine(&cfg, root, &fake);
    let e = m.entry("/opt/link/..", "rustc").unwrap();
    assert!(e.is_some(), "the command in /srv is found");
    assert_eq!(
        m.entry("/opt/link/..", "tool").unwrap().map(|e| e.name),
        None
    );
    let listing = m.list("/opt/link/..").unwrap();
    assert_eq!(listing.real, "/srv", "{listing:?}");
    assert_eq!(
        listing
            .entries
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>(),
        ["rustc"]
    );
}
