//! The sandbox's own tests.

use super::*;
use std::collections::HashMap;

/// An environment of just these variables.
fn env_of(vars: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> + use<> {
    let vars: HashMap<String, OsString> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.as_os_str().to_owned()))
        .collect();
    move |name| vars.get(name).cloned()
}

/// A job in DIR whose project is PROJECT, and its config.
fn job_in(dir: &Path, project: &Path) -> Config {
    let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
    cfg.jobs = dir.join("jobs");
    cfg.tools.git = "git".into();
    let job = Job::new(&cfg, "job");
    std::fs::create_dir_all(&job.dir).unwrap();
    std::fs::write(job.path("project"), project.to_str().unwrap()).unwrap();
    cfg
}

/// A machine in DIR: its own paths are `system`, which is there, and
/// `gone`, which isn't; its resolver config links to `resolved/stub`;
/// its /tmp is `tmp`.
pub(crate) fn machine_in(dir: &Path) -> Machine {
    std::fs::create_dir_all(dir.join("system")).unwrap();
    std::fs::create_dir_all(dir.join("resolved")).unwrap();
    std::fs::write(dir.join("resolved/stub"), "nameserver 127.0.0.53\n").unwrap();
    std::os::unix::fs::symlink(dir.join("resolved/stub"), dir.join("resolv.conf")).unwrap();
    Machine {
        system: vec![dir.join("system"), dir.join("gone")],
        resolver: dir.join("resolv.conf"),
        tmp: dir.join("tmp"),
    }
}

#[test]
fn heavy_slots_are_readable_only_when_configured_and_present() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let mut cfg = job_in(dir.path(), &project);
    let machine = machine_in(dir.path());
    let home = dir.path().join("home");
    let env = env_of(&[("HOME", &home)]);
    let heavy = dir.path().join("heavy");
    std::fs::create_dir(&heavy).unwrap();
    std::fs::write(heavy.join("slot1"), "").unwrap();
    let scratch = dir.path().join("scratch");
    let profile = |cfg: &Config| {
        let job = Job::new(cfg, "job");
        flags(cfg, &job, &scratch, &env, &machine)
            .unwrap()
            .flags
            .join("\n")
    };
    assert!(
        cfg.heavy_dir.is_none(),
        "old configs need no heavy directory"
    );
    assert!(!profile(&cfg).contains(&format!("\"{}\"=", heavy.display())));
    cfg.heavy_dir = Some(heavy.clone());
    assert!(profile(&cfg).contains(&format!("\"{}\"=\"read\"", heavy.display())));
    let missing = dir.path().join("missing-heavy");
    cfg.heavy_dir = Some(missing.clone());
    assert!(!profile(&cfg).contains(&format!("\"{}\"=", missing.display())));
}

fn deny(path: &Path) -> String {
    format!("\"{}\"=\"deny\"", path.display())
}

#[test]
fn grants_are_exact_git_metadata_is_explicit_and_the_login_hidden() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    assert!(
        Command::new("git")
            .arg("init")
            .arg(&project)
            .status()
            .unwrap()
            .success()
    );
    let cfg = job_in(dir.path(), &project);
    let job = Job::new(&cfg, "job");
    let machine = machine_in(dir.path());
    let home = dir.path().join("home");
    let env = env_of(&[("HOME", &home)]);
    let flags = flags(&cfg, &job, &dir.path().join("scratch"), &env, &machine)
        .unwrap()
        .flags;
    assert_eq!(flags[0], "-c", "no exec flags for a repository");
    let config = flags.join("\n");
    assert!(config.contains(&format!("\"{}\"=\"write\"", project.join(".git").display())));
    assert!(config.contains(&format!("\"{}\"=\"write\"", job.dir.display())));
    assert!(config.contains(&format!("\"{}\"=\"write\"", home.join(".cargo").display())));
    // Codex's home, its login in it, is denied whole.
    assert!(config.contains(&deny(&home.join(".codex"))));
    assert!(!config.contains(&deny(&home.join(".codex/auth.json"))));
    // A sandbox mode anywhere would put the legacy policy in its place.
    assert!(!config.contains("sandbox_mode"));
}

#[test]
fn the_profile_is_one_no_config_can_name() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("plain");
    std::fs::create_dir_all(&project).unwrap();
    let cfg = job_in(dir.path(), &project);
    let job = Job::new(&cfg, "job");
    let machine = machine_in(dir.path());
    let home = dir.path().join("home");
    let env = env_of(&[("HOME", &home)]);
    let profile = |flags: &[String]| {
        let selected = flags
            .iter()
            .find_map(|f| f.strip_prefix("default_permissions=\""))
            .and_then(|f| f.strip_suffix('"'))
            .expect("a profile")
            .to_owned();
        let named: Vec<&String> = flags
            .iter()
            .filter(|f| f.starts_with("permissions."))
            .collect();
        assert_eq!(named.len(), 2, "{named:?}");
        for f in named {
            assert!(f.starts_with(&format!("permissions.{selected}.")), "{f}");
        }
        selected
    };
    let scratch = dir.path().join("scratch");
    let first = profile(&flags(&cfg, &job, &scratch, &env, &machine).unwrap().flags);
    let again = profile(&flags(&cfg, &job, &scratch, &env, &machine).unwrap().flags);
    assert_ne!(first, "proj");
    assert!(
        first.starts_with("proj-") && first.len() == "proj-".len() + 32,
        "{first}"
    );
    assert_ne!(first, again, "each start of Codex names its own");
}

#[test]
fn a_project_with_no_repository_starts_and_may_make_one() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("plain");
    std::fs::create_dir_all(&project).unwrap();
    let cfg = job_in(dir.path(), &project);
    let job = Job::new(&cfg, "job");
    let machine = machine_in(dir.path());
    let env = env_of(&[("HOME", &dir.path().join("home"))]);
    let flags = flags(&cfg, &job, &dir.path().join("scratch"), &env, &machine)
        .unwrap()
        .flags;
    assert_eq!(flags[0], "--skip-git-repo-check");
    // Its .git is granted, for a `git init`, but not made: an empty one
    // would read as a broken repository.
    let config = flags.join("\n");
    assert!(config.contains(&format!("\"{}\"=\"write\"", project.join(".git").display())));
    assert!(!project.join(".git").exists());
}

#[test]
fn codex_home_is_found_as_codex_finds_it() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let real = dir.path().join("real-codex");
    std::fs::create_dir_all(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let empty = PathBuf::new();
    // Unset or empty: HOME's .codex.
    assert_eq!(
        codex_home(&env_of(&[("HOME", &home)])).unwrap(),
        home.join(".codex")
    );
    assert_eq!(
        codex_home(&env_of(&[("HOME", &home), ("CODEX_HOME", &empty)])).unwrap(),
        home.join(".codex")
    );
    // Set: canonical, through a symlink or a relative path.
    let real = real.canonicalize().unwrap();
    assert_eq!(codex_home(&env_of(&[("CODEX_HOME", &link)])).unwrap(), real);
    assert_eq!(
        codex_home(&env_of(&[("CODEX_HOME", &link.join("../real-codex"))])).unwrap(),
        real
    );
    assert_eq!(
        codex_home(&env_of(&[("CODEX_HOME", Path::new("."))])).unwrap(),
        std::env::current_dir().unwrap().canonicalize().unwrap()
    );
    // Codex won't start on a CODEX_HOME that isn't a directory; nor does the job.
    assert!(codex_home(&env_of(&[("CODEX_HOME", &dir.path().join("missing"))])).is_err());
    // A relative HOME is made absolute; none at all stops the job.
    assert_eq!(
        codex_home(&env_of(&[("HOME", Path::new("rel"))])).unwrap(),
        std::env::current_dir().unwrap().join("rel/.codex")
    );
    assert!(codex_home(&env_of(&[("HOME", &empty)])).is_err());
}

#[test]
fn the_login_is_denied_where_codex_keeps_it_and_where_it_leads() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real-codex");
    std::fs::create_dir_all(&real).unwrap();
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    std::fs::write(real.join("elsewhere.json"), "{}").unwrap();
    std::os::unix::fs::symlink(real.join("elsewhere.json"), real.join("auth.json")).unwrap();
    let env = env_of(&[("HOME", &dir.path().join("home")), ("CODEX_HOME", &link)]);
    let paths = denied(&env).unwrap();
    let real = real.canonicalize().unwrap();
    assert!(paths.contains(&real.join("auth.json")), "{paths:?}");
    assert!(paths.contains(&real.join("elsewhere.json")), "{paths:?}");
    // Not yet there, it's denied all the same.
    let env = env_of(&[("HOME", &dir.path().join("home"))]);
    let paths = denied(&env).unwrap();
    assert!(
        paths.contains(&dir.path().join("home/.codex/auth.json")),
        "{paths:?}"
    );
}

#[test]
fn the_job_reads_the_system_its_token_its_toolchains_and_gits_config_and_nothing_else() {
    use std::os::unix::net::UnixListener;
    let dir = tempfile::tempdir().unwrap();
    let machine = machine_in(dir.path());
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".config/git")).unwrap();
    std::fs::create_dir_all(home.join(".rustup")).unwrap();
    std::fs::write(home.join(".gitconfig"), "[user]\n").unwrap();
    // A socket where git's XDG config would be: not a file, not read.
    let _server = UnixListener::bind(home.join(".config/git/config")).unwrap();
    let token = dir.path().join("gh-token/workers.sock");
    let env = env_of(&[("HOME", &home), ("GH_TOKEN_SOCKET", &token)]);
    let paths = readable(&env, &machine).unwrap();
    assert_eq!(
        paths,
        [
            dir.path().join("system"),
            dir.path().join("resolved/stub").canonicalize().unwrap(),
            dir.path().join("gh-token"),
            home.join(".rustup"),
            home.join(".gitconfig"),
        ]
    );
    // GIT_CONFIG_GLOBAL is the only global config git reads, and
    // RUSTUP_HOME where rustup's toolchains are, when set; a resolver
    // config that isn't a file adds nothing.
    let global = dir.path().join("global");
    std::fs::write(&global, "").unwrap();
    let toolchains = dir.path().join("toolchains");
    std::fs::create_dir_all(&toolchains).unwrap();
    let env = env_of(&[
        ("HOME", &home),
        ("GIT_CONFIG_GLOBAL", &global),
        ("RUSTUP_HOME", &toolchains),
    ]);
    let machine = Machine {
        resolver: dir.path().join("missing"),
        ..machine
    };
    assert_eq!(
        readable(&env, &machine).unwrap(),
        [dir.path().join("system"), toolchains, global]
    );
    // Nor one of the CLI's platform defaults below it, where the CLI's
    // own helpers mount it again.
    for path in SYSTEM {
        for default in PLATFORM {
            assert!(
                path == default || !Path::new(default).starts_with(path),
                "{path} holds {default}"
            );
        }
    }
    // The real list holds none of the places a user's process binds
    // its sockets, nor a dir above one.
    for shared in [
        "/tmp",
        "/var/tmp",
        "/run/user",
        "/home",
        "/data",
        "/dev/shm",
    ] {
        for path in SYSTEM {
            let path = Path::new(path);
            assert!(
                !path.starts_with(shared) && !Path::new(shared).starts_with(path),
                "{}",
                path.display()
            );
        }
    }
}

#[test]
fn the_profile_starts_from_nothing_but_its_grants() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("plain");
    std::fs::create_dir_all(&project).unwrap();
    let cfg = job_in(dir.path(), &project);
    let job = Job::new(&cfg, "job");
    let machine = machine_in(dir.path());
    let home = dir.path().join("home");
    let env = env_of(&[("HOME", &home)]);
    let flags = flags(&cfg, &job, &dir.path().join("scratch"), &env, &machine)
        .unwrap()
        .flags;
    let filesystem = flags
        .iter()
        .find(|f| f.contains(".filesystem="))
        .expect("a filesystem table");
    assert!(!filesystem.contains(":root"), "{filesystem}");
    assert!(!filesystem.contains(":minimal"), "{filesystem}");
    assert!(filesystem.contains(&format!(
        "\"{}\"=\"read\"",
        dir.path().join("system").display()
    )));
    assert!(!filesystem.contains(&dir.path().join("gone").display().to_string()));
    assert!(
        filesystem.contains(&format!("\"{}\"=\"write\"", machine.tmp.display())),
        "{filesystem}"
    );
    // Nothing of home is there but its grants: not home itself.
    assert!(!filesystem.contains(&format!("\"{}\"=", home.display())));
    // Commands build through the daemon, the store being read-only.
    assert!(flags.contains(&r#"shell_environment_policy.set.NIX_REMOTE="daemon""#.to_owned()));
}

/// The filesystem table's entry for PATH, if it has one.
fn entry(flags: &[String], path: &Path) -> Option<String> {
    let filesystem = flags.iter().find(|f| f.contains(".filesystem="))?;
    let key = format!("\"{}\"=\"", path.display());
    let at = filesystem.find(&key)? + key.len();
    Some(filesystem[at..].split('"').next()?.to_owned())
}

#[test]
fn what_codex_and_the_job_use_under_tmp_is_kept_with_its_own_access() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let machine = machine_in(dir.path());
    let tmp = machine.tmp.clone();
    // The project a worktree under /tmp, its repository's git metadata
    // there too; the job's state and scratch there.
    let repo = tmp.join("repo");
    let project = tmp.join("project");
    let git = |args: &[&str]| {
        assert!(Command::new("git").args(args).status().unwrap().success());
    };
    git(&["init", "-q", repo.to_str().unwrap()]);
    git(&[
        "-C",
        repo.to_str().unwrap(),
        "-c",
        "user.name=T",
        "-c",
        "user.email=t@t",
        "commit",
        "-q",
        "--allow-empty",
        "-m",
        "i",
    ]);
    git(&[
        "-C",
        repo.to_str().unwrap(),
        "worktree",
        "add",
        "-q",
        project.to_str().unwrap(),
    ]);
    let mut cfg = job_in(&tmp, &project);
    cfg.scratch = tmp.join("scratch");
    let job = Job::new(&cfg, "job");
    let scratch = cfg.scratch.join("job");
    // HOME there, so cargo's and nix's caches are; Codex's home there
    // through a symlink from outside; git's config a file right in
    // /tmp; rustup's toolchains through a symlink in /tmp to outside;
    // the token's socket in a dir there.
    let home = tmp.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(tmp.join("codex-home")).unwrap();
    std::fs::write(tmp.join("codex-home/auth.json"), "{}").unwrap();
    let link = dir.path().join("codex-link");
    symlink(tmp.join("codex-home"), &link).unwrap();
    let global = tmp.join("gitconfig");
    std::fs::write(&global, "[user]\n").unwrap();
    let toolchains = dir.path().join("toolchains");
    std::fs::create_dir_all(&toolchains).unwrap();
    let rustup = tmp.join("rustup");
    symlink(&toolchains, &rustup).unwrap();
    std::fs::create_dir_all(tmp.join("gh-token")).unwrap();
    let token = tmp.join("gh-token/workers.sock");
    // What else is in the machine's /tmp: none of the job's.
    std::fs::create_dir_all(tmp.join("tmux-1001")).unwrap();
    let env = env_of(&[
        ("HOME", &home),
        ("CODEX_HOME", &link),
        ("GIT_CONFIG_GLOBAL", &global),
        ("RUSTUP_HOME", &rustup),
        ("GH_TOKEN_SOCKET", &token),
    ]);
    let first = flags(&cfg, &job, &scratch, &env, &machine).unwrap();
    let kept = [
        tmp.join("codex-home"),
        tmp.join("gh-token"),
        global.clone(),
        home.join(".cache/nix"),
        home.join(".cargo"),
        tmp.join("jobs/job"),
        project.clone(),
        repo.join(".git"),
        rustup.clone(),
        scratch.clone(),
    ];
    let mut sorted = kept.to_vec();
    sorted.sort();
    assert_eq!(first.kept, sorted);
    // Each has its own entry, as it would outside /tmp, so the job's
    // /tmp grant doesn't reach it: Codex's home stays denied (its
    // login with it), and what's read stays read.
    let access = |path: &Path| entry(&first.flags, path);
    assert_eq!(access(&tmp).as_deref(), Some("write"));
    assert_eq!(access(&tmp.join("codex-home")).as_deref(), Some("deny"));
    assert_eq!(access(&tmp.join("codex-home/auth.json")), None);
    for read in [&global, &rustup, &tmp.join("gh-token")] {
        assert_eq!(access(read).as_deref(), Some("read"), "{}", read.display());
    }
    for write in [
        &project,
        &repo.join(".git"),
        &scratch,
        &tmp.join("jobs/job"),
        &home.join(".cargo"),
    ] {
        assert_eq!(
            access(write).as_deref(),
            Some("write"),
            "{}",
            write.display()
        );
    }
    // Not HOME, nor anything else of the machine's /tmp.
    assert_eq!(access(&home), None);
    assert_eq!(access(&tmp.join("tmux-1001")), None);
    // The next start keeps the same.
    let again = flags(&cfg, &job, &scratch, &env, &machine).unwrap();
    assert_eq!(again.kept, first.kept);
    // /tmp itself can't be kept: the job's own is there.
    let env = env_of(&[("HOME", &home), ("CARGO_HOME", &tmp)]);
    let error = flags(&cfg, &job, &scratch, &env, &machine).err().unwrap();
    assert!(error.to_string().contains("the job's own"), "{error}");
}

#[test]
fn a_path_through_tmp_by_any_symlink_leads_where_it_did() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let machine = machine_in(dir.path());
    let tmp = machine.tmp.clone();
    let project = dir.path().join("plain");
    std::fs::create_dir_all(&project).unwrap();
    let cfg = job_in(dir.path(), &project);
    let job = Job::new(&cfg, "job");
    let scratch = dir.path().join("scratch");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(tmp.join("tmux-1001")).unwrap();
    // CODEX_HOME a symlink in /tmp to a dir outside it.
    let real = dir.path().join("codex-real");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::write(real.join("auth.json"), "{}").unwrap();
    symlink(&real, tmp.join("codex-link")).unwrap();
    // RUSTUP_HOME through a chain: outside, to /tmp, to outside.
    let toolchains = dir.path().join("toolchains");
    std::fs::create_dir_all(&toolchains).unwrap();
    symlink(&toolchains, tmp.join("rustup-hop")).unwrap();
    let rustup = dir.path().join("rustup-link");
    symlink(tmp.join("rustup-hop"), &rustup).unwrap();
    // git's config a symlink in /tmp to a file outside it.
    let global = dir.path().join("gitconfig");
    std::fs::write(&global, "[user]\n").unwrap();
    symlink(&global, tmp.join("gitconfig-link")).unwrap();
    let env = env_of(&[
        ("HOME", &home),
        ("CODEX_HOME", &tmp.join("codex-link")),
        ("RUSTUP_HOME", &rustup),
        ("GIT_CONFIG_GLOBAL", &tmp.join("gitconfig-link")),
    ]);
    let first = flags(&cfg, &job, &scratch, &env, &machine).unwrap();
    // Codex starts with where its home leads, which is there in the
    // job: not the link, which the job's /tmp hides. It's denied there,
    // its login with it.
    let real = real.canonicalize().unwrap();
    assert_eq!(first.codex_home.as_ref(), Some(&real));
    assert_eq!(entry(&first.flags, &real).as_deref(), Some("deny"));
    // Each hop through /tmp is kept, read as what it leads to is.
    assert_eq!(
        first.kept,
        [tmp.join("gitconfig-link"), tmp.join("rustup-hop")]
    );
    assert_eq!(
        entry(&first.flags, &tmp.join("rustup-hop")).as_deref(),
        Some("read")
    );
    assert_eq!(
        entry(&first.flags, &tmp.join("gitconfig-link")).as_deref(),
        Some("read")
    );
    assert_eq!(entry(&first.flags, &tmp.join("codex-link")), None);
    assert_eq!(entry(&first.flags, &tmp.join("tmux-1001")), None);
    // A resumed start, the same.
    let again = flags(&cfg, &job, &scratch, &env, &machine).unwrap();
    assert_eq!(
        (again.kept, again.codex_home),
        (first.kept, first.codex_home)
    );

    // CODEX_HOME through a chain out of /tmp and back in: Codex starts
    // with where it ends, kept and denied there.
    std::fs::create_dir_all(tmp.join("codex-home")).unwrap();
    let hop = dir.path().join("codex-hop");
    symlink(tmp.join("codex-home"), &hop).unwrap();
    symlink(&hop, tmp.join("codex-chain")).unwrap();
    let env = env_of(&[("HOME", &home), ("CODEX_HOME", &tmp.join("codex-chain"))]);
    let chained = flags(&cfg, &job, &scratch, &env, &machine).unwrap();
    assert_eq!(chained.codex_home, Some(tmp.join("codex-home")));
    assert_eq!(chained.kept, [tmp.join("codex-home")]);
    assert_eq!(
        entry(&chained.flags, &tmp.join("codex-home")).as_deref(),
        Some("deny")
    );

    // Unset, HOME's .codex, which Codex doesn't resolve: a hop of it
    // through /tmp is kept, and denied as the home is.
    let other = dir.path().join("other-codex");
    std::fs::create_dir_all(&other).unwrap();
    symlink(&other, tmp.join("dot-codex")).unwrap();
    symlink(tmp.join("dot-codex"), home.join(".codex")).unwrap();
    let env = env_of(&[("HOME", &home)]);
    let unset = flags(&cfg, &job, &scratch, &env, &machine).unwrap();
    assert_eq!(unset.codex_home, None);
    assert_eq!(unset.kept, [tmp.join("dot-codex")]);
    assert_eq!(
        entry(&unset.flags, &tmp.join("dot-codex")).as_deref(),
        Some("deny")
    );
    assert_eq!(
        entry(&unset.flags, &other.canonicalize().unwrap()).as_deref(),
        Some("deny")
    );
}

#[test]
fn a_walk_looks_up_in_tmp_as_the_kernel_does() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let tmp = dir.path().join("tmp").canonicalize().unwrap_or_else(|_| {
        std::fs::create_dir_all(dir.path().join("tmp")).unwrap();
        dir.path().join("tmp").canonicalize().unwrap()
    });
    let out = dir.path().canonicalize().unwrap().join("out");
    std::fs::create_dir_all(out.join("sub")).unwrap();
    // Named in /tmp, through a relative link in it to outside.
    symlink("../out", tmp.join("rel")).unwrap();
    let (lookups, end) = lookups_in(&tmp.join("rel/sub"), &tmp).unwrap();
    assert_eq!(lookups, [tmp.join("rel/sub")]);
    assert_eq!(end, out.join("sub"));
    // Outside, a relative link into /tmp, and `.` on the way.
    std::fs::create_dir_all(tmp.join("in")).unwrap();
    symlink("tmp/./in", dir.path().join("into")).unwrap();
    let (lookups, end) = lookups_in(&dir.path().join("into/./x"), &tmp).unwrap();
    assert_eq!(lookups, [tmp.join("in/x")]);
    assert_eq!(end, tmp.join("in/x"));
    // Never in /tmp: nothing to look up there.
    let (lookups, _) = lookups_in(&out.join("sub"), &tmp).unwrap();
    assert!(lookups.is_empty());
    // Back up (..) after a name in /tmp: no path there says where
    // that leads, so the job doesn't start.
    assert!(lookups_in(&tmp.join("in/../rel"), &tmp).is_err());
    // A loop of links, nor.
    symlink(tmp.join("b"), tmp.join("a")).unwrap();
    symlink(tmp.join("a"), tmp.join("b")).unwrap();
    assert!(lookups_in(&tmp.join("a"), &tmp).is_err());
}

#[test]
fn nothing_the_job_left_in_its_tmp_moves_what_it_keeps() {
    use std::os::unix::fs::symlink;
    let dir = tempfile::tempdir().unwrap();
    let machine = machine_in(dir.path());
    let private = private_tmp(&dir.path().join("scratch")).unwrap();
    let kept = [machine.tmp.join("codex-home"), machine.tmp.join("a/b")];
    // Not there yet, or a dir bwrap made: kept where it is.
    check_kept(&private, &machine, &kept).unwrap();
    std::fs::create_dir_all(private.join("codex-home")).unwrap();
    std::fs::create_dir_all(private.join("a/b")).unwrap();
    check_kept(&private, &machine, &kept).unwrap();
    // A symlink where it goes, or above it, and Codex doesn't start.
    std::fs::remove_dir(private.join("codex-home")).unwrap();
    symlink(dir.path(), private.join("codex-home")).unwrap();
    assert!(check_kept(&private, &machine, &kept[..1]).is_err());
    std::fs::remove_dir_all(private.join("a")).unwrap();
    symlink(dir.path(), private.join("a")).unwrap();
    assert!(check_kept(&private, &machine, &kept[1..]).is_err());
}

#[test]
fn the_jobs_tmp_is_its_own() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let scratch = dir.path().join("scratch");
    let tmp = private_tmp(&scratch).unwrap();
    assert_eq!(tmp, scratch.join("tmp"));
    let meta = tmp.symlink_metadata().unwrap();
    assert!(meta.is_dir());
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
    // Made again (the next turn's start), it keeps what's there.
    std::fs::write(tmp.join("kept"), "").unwrap();
    assert_eq!(private_tmp(&scratch).unwrap(), tmp);
    assert!(tmp.join("kept").exists());
    // Never a symlink to somewhere else.
    let other = dir.path().join("other");
    std::fs::create_dir_all(other.join("tmp-target")).unwrap();
    std::os::unix::fs::symlink(other.join("tmp-target"), other.join("tmp")).unwrap();
    assert!(private_tmp(&other).is_err());
    // Codex runs with it over /tmp, TMPDIR saying so.
    let argv: Vec<String> = wrap(&tmp, &[], None, Path::new("/bin/codex"))
        .into_iter()
        .map(|a| a.into_string().unwrap())
        .collect();
    let tmp = tmp.display().to_string();
    assert_eq!(
        argv,
        [
            "--dev-bind",
            "/",
            "/",
            "--bind",
            tmp.as_str(),
            "/tmp",
            "--setenv",
            "TMPDIR",
            "/tmp",
            "--die-with-parent",
            "--",
            "/bin/codex"
        ]
    );
}
