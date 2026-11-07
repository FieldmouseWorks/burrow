//! A Codex job's sandbox: the pinned CLI's named permissions profile, made
//! by `proj _codex` once the login shell and the App's env have set the
//! environment Codex itself starts in, so the paths it names are the ones
//! Codex and the job's commands find.
//!
//! The project, scratch, the job's own dir and git's common dir (worktrees
//! made in scratch write there; an explicit grant also lifts the automatic
//! .git protection) are writable, and the rest of the machine is read-only
//! but what [`denied`] keeps out of sight. The network is on, which is also
//! what lets a command connect() to the gh-token socket under /run/gh-token:
//! the pinned CLI's seccomp has no finer switch for unix sockets. Commands
//! keep no_new_privs, since the filesystem isn't all writable, so sudo
//! can't elevate.

use crate::{config::Config, job::Job, read};
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The environment Codex starts in, a variable at a time.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// A variable that says something: unset and empty read alike, as Codex,
/// systemd and tmux read them.
fn var(env: Env, name: &str) -> Option<PathBuf> {
    env(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// HOME, made absolute as Codex makes it. Unset, the job doesn't start,
/// rather than guess where Codex's login is.
fn home(env: Env) -> io::Result<PathBuf> {
    std::path::absolute(var(env, "HOME").ok_or_else(|| io::Error::other("HOME isn't set"))?)
}

/// Codex's own home, as the pinned CLI's find_codex_home finds it:
/// CODEX_HOME, a directory, canonicalized; else HOME's `.codex`.
fn codex_home(env: Env) -> io::Result<PathBuf> {
    match var(env, "CODEX_HOME") {
        Some(dir) if dir.is_dir() => dir.canonicalize(),
        Some(dir) => Err(io::Error::other(format!(
            "CODEX_HOME points to {}, which isn't a directory",
            dir.display()
        ))),
        None => Ok(home(env)?.join(".codex")),
    }
}

/// What the job's commands may not read, though the rest of the machine is
/// theirs to: Codex's login, which they have no use for, and the doors to
/// running something outside the sandbox, where neither its filesystem
/// grants nor no_new_privs reach. Those are the user's runtime dir (its
/// D-Bus, the systemd manager's private and varlink sockets, podman's) and
/// tmux's servers. Each goes in as named and as it resolves, since a
/// symlink's own path and its target are two entries to the CLI.
///
/// Only the login is denied whether or not it's there yet. The CLI masks a
/// denied path that doesn't exist by making an empty file in its place when
/// that place is writable, as /tmp is, and an empty `tmux-UID` there would
/// keep tmux from starting at all, sandbox or not. What isn't there has no
/// server behind it to reach.
fn denied(env: Env, uid: u32) -> io::Result<Vec<PathBuf>> {
    let runtime = [
        var(env, "XDG_RUNTIME_DIR"),
        Some(PathBuf::from(format!("/run/user/{uid}"))),
        Some(
            var(env, "TMUX_TMPDIR")
                .unwrap_or_else(|| "/tmp".into())
                .join(format!("tmux-{uid}")),
        ),
    ];
    let mut paths = vec![codex_home(env)?.join("auth.json")];
    for path in runtime.into_iter().flatten() {
        let path = std::path::absolute(path)?;
        if path.exists() {
            paths.push(path);
        }
    }
    let resolved: Vec<PathBuf> = paths.iter().filter_map(|p| p.canonicalize().ok()).collect();
    paths.extend(resolved);
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// A name no config of the user's or a project's can know beforehand, so
/// none can widen the profile: the CLI merges a profile's tables across
/// every config layer, `-c`'s included, and follows its `extends`. Each
/// start of Codex, fresh or resumed, gets its own.
fn profile_name() -> io::Result<String> {
    let mut bytes = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().fold("proj-".to_owned(), |mut name, b| {
        name.push_str(&format!("{b:02x}"));
        name
    }))
}

/// The flags that give Codex the job's sandbox, all of them `exec`'s, to go
/// before `resume`. A project that isn't a repository yet gets its own
/// `.git` granted without making it, so the job can `git init` there (the
/// pinned CLI grants a path that doesn't exist yet), and Codex is told to
/// start outside one.
pub fn flags(
    cfg: &Config,
    job: &Job,
    scratch: &Path,
    env: Env,
    uid: u32,
) -> io::Result<Vec<String>> {
    let project = PathBuf::from(read(&job.path("project")).unwrap_or_default());
    // proj await/done and commands' wait files write only this job's state.
    let mut roots = vec![project.clone(), scratch.to_owned(), job.dir.clone()];
    let git = Command::new(&cfg.tools.git)
        .arg("-C")
        .arg(&project)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()?;
    let (metadata, mut flags) = if git.status.success() {
        let common = String::from_utf8_lossy(&git.stdout).trim().to_owned();
        (PathBuf::from(common), Vec::new())
    } else {
        (
            project.join(".git"),
            vec!["--skip-git-repo-check".to_owned()],
        )
    };
    let home = home(env)?;
    for (name, default) in [
        ("CARGO_HOME", home.join(".cargo")),
        ("XDG_CACHE_HOME", home.join(".cache")),
    ] {
        let root = std::path::absolute(var(env, name).unwrap_or(default))?;
        std::fs::create_dir_all(&root)?;
        roots.push(if name == "XDG_CACHE_HOME" {
            root.join("nix")
        } else {
            root
        });
    }
    for root in &roots {
        std::fs::create_dir_all(root)?;
    }
    roots.push(metadata);
    let quoted = |p: &Path| serde_json::to_string(&p.to_string_lossy()).expect("string");
    let entries = roots
        .iter()
        .map(|p| format!("{}=\"write\"", quoted(p)))
        .chain(
            denied(env, uid)?
                .iter()
                .map(|p| format!("{}=\"deny\"", quoted(p))),
        )
        .collect::<Vec<_>>()
        .join(",");
    let name = profile_name()?;
    for config in [
        r#"approval_policy="never""#.to_owned(),
        format!("default_permissions=\"{name}\""),
        format!(
            "permissions.{name}.filesystem={{\":root\"=\"read\",\"/tmp\"=\"write\",{entries}}}"
        ),
        format!("permissions.{name}.network.enabled=true"),
        // The login shell's App env and git hook config must reach commands.
        r#"shell_environment_policy.inherit="all""#.to_owned(),
        "shell_environment_policy.ignore_default_excludes=true".to_owned(),
    ] {
        flags.extend(["-c".into(), config]);
    }
    Ok(flags)
}

#[cfg(test)]
mod tests {
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
        let home = dir.path().join("home");
        let env = env_of(&[("HOME", &home)]);
        let flags = flags(&cfg, &job, &dir.path().join("scratch"), &env, u32::MAX).unwrap();
        assert_eq!(flags[0], "-c", "no exec flags for a repository");
        let config = flags.join("\n");
        assert!(config.contains(&format!("\"{}\"=\"write\"", project.join(".git").display())));
        assert!(config.contains(&format!("\"{}\"=\"write\"", job.dir.display())));
        assert!(config.contains(&format!("\"{}\"=\"write\"", home.join(".cargo").display())));
        assert!(config.contains(&deny(&home.join(".codex/auth.json"))));
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
        let first = profile(&flags(&cfg, &job, &scratch, &env, u32::MAX).unwrap());
        let again = profile(&flags(&cfg, &job, &scratch, &env, u32::MAX).unwrap());
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
        let env = env_of(&[("HOME", &dir.path().join("home"))]);
        let flags = flags(&cfg, &job, &dir.path().join("scratch"), &env, u32::MAX).unwrap();
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
        let paths = denied(&env, u32::MAX).unwrap();
        let real = real.canonicalize().unwrap();
        assert!(paths.contains(&real.join("auth.json")), "{paths:?}");
        assert!(paths.contains(&real.join("elsewhere.json")), "{paths:?}");
        // Not yet there, it's denied all the same.
        let env = env_of(&[("HOME", &dir.path().join("home"))]);
        let paths = denied(&env, u32::MAX).unwrap();
        assert!(
            paths.contains(&dir.path().join("home/.codex/auth.json")),
            "{paths:?}"
        );
    }

    #[test]
    fn the_user_manager_and_tmux_are_out_of_reach() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let runtime = dir.path().join("runtime");
        let tmux = dir.path().join("tmux");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::create_dir_all(tmux.join("tmux-4242")).unwrap();
        let env = env_of(&[
            ("HOME", &home),
            ("XDG_RUNTIME_DIR", &runtime),
            ("TMUX_TMPDIR", &tmux),
        ]);
        let paths = denied(&env, 4242).unwrap();
        // The runtime dir holds the user's D-Bus and the systemd manager's
        // private socket.
        assert!(paths.contains(&runtime), "{paths:?}");
        assert!(paths.contains(&tmux.join("tmux-4242")), "{paths:?}");
        // What isn't there isn't masked: that would make an empty file in a
        // writable /tmp, where tmux's own dir goes.
        let paths = denied(&env_of(&[("HOME", &home), ("TMUX_TMPDIR", &tmux)]), 4343).unwrap();
        assert_eq!(paths, [home.join(".codex/auth.json")]);
        // The manager's own runtime dir counts, whatever XDG_RUNTIME_DIR says.
        let uid = rustix::process::getuid().as_raw();
        let mine = PathBuf::from(format!("/run/user/{uid}"));
        if mine.exists() {
            let paths = denied(&env_of(&[("HOME", &home)]), uid).unwrap();
            assert!(paths.contains(&mine), "{paths:?}");
        }
        // And all of them reach the profile.
        let project = dir.path().join("plain");
        std::fs::create_dir_all(&project).unwrap();
        let cfg = job_in(dir.path(), &project);
        let job = Job::new(&cfg, "job");
        let config = flags(&cfg, &job, &dir.path().join("scratch"), &env, 4242)
            .unwrap()
            .join("\n");
        assert!(config.contains(&deny(&runtime)), "{config}");
        assert!(config.contains(&deny(&tmux.join("tmux-4242"))), "{config}");
    }
}
