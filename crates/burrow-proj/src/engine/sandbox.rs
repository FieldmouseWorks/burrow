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

/// Where the host keeps what [`denied`] looks for, and whose it is: the
/// user, tmux's default base dir (tmux puts its servers in `tmux-UID` there
/// unless TMUX_TMPDIR says elsewhere), and the kernel's table of bound unix
/// sockets.
pub struct Host {
    pub uid: u32,
    pub tmp: PathBuf,
    pub sockets: PathBuf,
}

impl Host {
    /// This machine, as the user proj runs as.
    pub fn real() -> Self {
        Self {
            uid: rustix::process::getuid().as_raw(),
            tmp: "/tmp".into(),
            sockets: "/proc/net/unix".into(),
        }
    }
}

/// What the job's commands may not read, though the rest of the machine is
/// theirs to: Codex's login, which they have no use for, and the doors to
/// running something outside the sandbox, where neither its filesystem
/// grants nor no_new_privs reach. With the network on, a command may
/// connect() to any unix socket it can see, so every server of the user's
/// own is a door, and each is hidden: the user's runtime dir (its D-Bus,
/// the systemd manager's private and varlink sockets, podman's), tmux's
/// socket dirs ([`tmux_dir`]) and every other socket a process of the
/// user's has bound ([`own_sockets`]). Each goes in as named and as it
/// resolves, since a symlink's own path and its target are two entries to
/// the CLI, and none under another: the CLI masks a denied dir read-only
/// first, and couldn't mount over a path inside it.
///
/// The CLI masks each path afresh for every command, a dir by mounting an
/// empty one over it, so a server started in a denied dir after this, mid
/// command even, stays out of sight. A path that isn't there when a command
/// starts gets an empty file in its place for that command, when that place
/// is writable, as /tmp is. So only the login is denied whether or not it's
/// there yet; tmux's dirs are made first, as tmux makes them, and every
/// other door is one that's there now.
fn denied(env: Env, host: &Host) -> io::Result<Vec<PathBuf>> {
    let uid = host.uid;
    let mut paths = vec![codex_home(env)?.join("auth.json")];
    for path in [
        var(env, "XDG_RUNTIME_DIR"),
        Some(PathBuf::from(format!("/run/user/{uid}"))),
    ]
    .into_iter()
    .flatten()
    {
        let path = std::path::absolute(path)?;
        if path.exists() {
            paths.push(path);
        }
    }
    for base in [Some(host.tmp.clone()), var(env, "TMUX_TMPDIR")]
        .into_iter()
        .flatten()
    {
        paths.extend(tmux_dir(&base, uid));
    }
    // A server tmux started with -S, or under a TMUX_TMPDIR the job's env
    // doesn't share, is found where it's bound; so is the one $TMUX names.
    let attached = var(env, "TMUX").and_then(|v| {
        let v = v.into_os_string().into_string().ok()?;
        Some(PathBuf::from(v.split(',').next()?))
    });
    let sockets = own_sockets(&host.sockets, uid)?
        .into_iter()
        .chain(attached.into_iter().filter(|p| is_own_socket(p, uid)));
    for socket in sockets {
        paths.extend(door(&socket, uid));
    }
    let resolved: Vec<PathBuf> = paths.iter().filter_map(|p| p.canonicalize().ok()).collect();
    paths.extend(resolved);
    paths.sort();
    paths.dedup();
    let outer = paths.clone();
    paths.retain(|p| !outer.iter().any(|o| o != p && p.starts_with(o)));
    Ok(paths)
}

/// The dir tmux keeps a user's servers in under BASE, made as tmux makes it
/// (its base resolved, `tmux-UID`, the user's alone) if it isn't there yet,
/// so it's there to deny for the whole turn: a server tmux starts there
/// later goes into a dir the job can't see. A base that isn't there, or a
/// `tmux-UID` that isn't a dir (tmux won't use a symlink), holds none.
fn tmux_dir(base: &Path, uid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = base.canonicalize().ok()?.join(format!("tmux-{uid}"));
    // Made or not (a base the user can't write), what's there decides.
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    dir.symlink_metadata().ok()?.is_dir().then_some(dir)
}

/// Every socket bound to a path by a process of the user's, from the
/// kernel's table (one line per socket after the header: seven fields, then
/// the path a socket is bound to, if any, after one space). It is the
/// user's when the socket file there is theirs. Unreadable, the job doesn't
/// start, rather than run with doors open.
fn own_sockets(table: &Path, uid: u32) -> io::Result<Vec<PathBuf>> {
    let text = std::fs::read_to_string(table)?;
    Ok(text
        .lines()
        .skip(1)
        .filter_map(bound_path)
        .map(PathBuf::from)
        .filter(|p| is_own_socket(p, uid))
        .collect())
}

/// The path a line of /proc/net/unix says its socket is bound to; abstract
/// ones (`@`) have none on the filesystem.
fn bound_path(line: &str) -> Option<&str> {
    let mut rest = line;
    for _ in 0..7 {
        rest = rest.trim_start();
        rest = &rest[rest.find(char::is_whitespace)?..];
    }
    rest.strip_prefix(' ').filter(|p| p.starts_with('/'))
}

fn is_own_socket(path: &Path, uid: u32) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    path.symlink_metadata()
        .is_ok_and(|m| m.file_type().is_socket() && m.uid() == uid)
}

/// What to deny for a server's socket: tmux's dir, if that's where it is,
/// so its other servers and those to come are hidden too; else the socket.
fn door(socket: &Path, uid: u32) -> Option<PathBuf> {
    let socket = socket.canonicalize().ok()?;
    match socket.parent() {
        Some(dir) if dir.file_name() == Some(format!("tmux-{uid}").as_ref()) => {
            Some(dir.to_owned())
        }
        _ => Some(socket),
    }
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
    host: &Host,
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
            denied(env, host)?
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
pub(crate) mod tests {
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

    /// A host in DIR, as UID: its own /tmp, and a socket table that lists
    /// nothing.
    pub(crate) fn host_in(dir: &Path, uid: u32) -> Host {
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&tmp).unwrap();
        let sockets = dir.join("unix");
        std::fs::write(&sockets, TABLE_HEADER).unwrap();
        Host { uid, tmp, sockets }
    }

    const TABLE_HEADER: &str = "Num       RefCount Protocol Flags    Type St Inode Path\n";

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
        let flags = flags(
            &cfg,
            &job,
            &dir.path().join("scratch"),
            &env,
            &host_in(dir.path(), u32::MAX),
        )
        .unwrap();
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
        let first =
            profile(&flags(&cfg, &job, &scratch, &env, &host_in(dir.path(), u32::MAX)).unwrap());
        let again =
            profile(&flags(&cfg, &job, &scratch, &env, &host_in(dir.path(), u32::MAX)).unwrap());
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
        let flags = flags(
            &cfg,
            &job,
            &dir.path().join("scratch"),
            &env,
            &host_in(dir.path(), u32::MAX),
        )
        .unwrap();
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
        let paths = denied(&env, &host_in(dir.path(), u32::MAX)).unwrap();
        let real = real.canonicalize().unwrap();
        assert!(paths.contains(&real.join("auth.json")), "{paths:?}");
        assert!(paths.contains(&real.join("elsewhere.json")), "{paths:?}");
        // Not yet there, it's denied all the same.
        let env = env_of(&[("HOME", &dir.path().join("home"))]);
        let paths = denied(&env, &host_in(dir.path(), u32::MAX)).unwrap();
        assert!(
            paths.contains(&dir.path().join("home/.codex/auth.json")),
            "{paths:?}"
        );
    }

    #[test]
    fn the_user_manager_is_out_of_reach() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let runtime = dir.path().join("runtime");
        std::fs::create_dir_all(&runtime).unwrap();
        let host = host_in(dir.path(), 4242);
        let env = env_of(&[("HOME", &home), ("XDG_RUNTIME_DIR", &runtime)]);
        // The runtime dir holds the user's D-Bus and the systemd manager's
        // private socket.
        let paths = denied(&env, &host).unwrap();
        assert!(paths.contains(&runtime), "{paths:?}");
        // The manager's own runtime dir counts, whatever XDG_RUNTIME_DIR says.
        let uid = rustix::process::getuid().as_raw();
        let mine = PathBuf::from(format!("/run/user/{uid}"));
        if mine.exists() {
            let paths = denied(&env_of(&[("HOME", &home)]), &host_in(dir.path(), uid)).unwrap();
            assert!(paths.contains(&mine), "{paths:?}");
        }
    }

    #[test]
    fn tmux_dirs_are_made_as_tmux_makes_them_and_hidden_before_any_server() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let custom = dir.path().join("custom");
        std::fs::create_dir_all(&custom).unwrap();
        let host = host_in(dir.path(), 4242);
        let env = env_of(&[("HOME", &home), ("TMUX_TMPDIR", &custom)]);
        let paths = denied(&env, &host).unwrap();
        // Neither dir was there: both are now, the user's alone, as tmux
        // would make them, and a server tmux starts there later is hidden.
        for base in [&host.tmp, &custom] {
            let made = base.canonicalize().unwrap().join("tmux-4242");
            assert!(paths.contains(&made), "{paths:?}");
            let meta = made.symlink_metadata().unwrap();
            assert!(meta.is_dir());
            assert_eq!(meta.permissions().mode() & 0o077, 0, "{made:?}");
        }
        // Made already, it's kept as it is.
        std::fs::write(custom.join("tmux-4242/default"), "").unwrap();
        assert!(
            denied(&env, &host)
                .unwrap()
                .contains(&custom.join("tmux-4242").canonicalize().unwrap())
        );
        assert!(custom.join("tmux-4242/default").exists());
        // A base that isn't there has no server; tmux won't use a tmux-UID
        // that isn't a dir, and none is put in its place.
        let missing = dir.path().join("missing");
        let paths = denied(
            &env_of(&[("HOME", &home), ("TMUX_TMPDIR", &missing)]),
            &host,
        )
        .unwrap();
        assert!(
            !missing.exists() && !paths.iter().any(|p| p.starts_with(&missing)),
            "{paths:?}"
        );
        let odd = host_in(&dir.path().join("odd"), 4343);
        std::fs::write(odd.tmp.join("tmux-4343"), "").unwrap();
        let paths = denied(&env_of(&[("HOME", &home)]), &odd).unwrap();
        assert_eq!(paths, [home.join(".codex/auth.json")]);
        assert!(odd.tmp.join("tmux-4343").is_file());
    }

    #[test]
    fn the_kernels_table_says_where_each_socket_is_bound() {
        let table = format!(
            "{TABLE_HEADER}\
             0000000000000000: 00000002 00000000 00010000 0001 01 12345 /run/a socket\n\
             0000000000000000: 00000002 00000000 00010000 0001 01    77 /tmp/b\n\
             0000000000000000: 00000002 00000000 00000000 0002 01 23456 @abstract/bus\n\
             0000000000000000: 00000003 00000000 00000000 0001 03 34567\n"
        );
        let paths: Vec<&str> = table.lines().skip(1).filter_map(bound_path).collect();
        assert_eq!(paths, ["/run/a socket", "/tmp/b"]);
    }

    #[test]
    fn every_server_of_the_users_own_is_hidden_wherever_it_is_bound() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path().canonicalize().unwrap();
        let uid = rustix::process::getuid().as_raw();
        let home = dir.join("home");
        let runtime = dir.join("runtime");
        // tmux -S, at a path of its own.
        let socket = dir.join("served.sock");
        // A tmux server under a TMUX_TMPDIR the job's env doesn't have.
        let elsewhere = dir.join("elsewhere/tmux-".to_owned() + &uid.to_string());
        // The one a job inside tmux would find in $TMUX.
        let attached = dir.join("attached.sock");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(&runtime).unwrap();
        let _served = UnixListener::bind(&socket).unwrap();
        let _custom = UnixListener::bind(elsewhere.join("custom")).unwrap();
        let _nested = UnixListener::bind(runtime.join("bus")).unwrap();
        // A socket file nobody serves any more, and one served by no one in
        // this table: neither is a door.
        drop(UnixListener::bind(dir.join("gone.sock")).unwrap());
        let host = Host {
            sockets: "/proc/net/unix".into(),
            ..host_in(&dir, uid)
        };
        let tmux = format!("{},4242,0", attached.display());
        let env = |name: &str| match name {
            "HOME" => Some(home.clone().into()),
            "XDG_RUNTIME_DIR" => Some(runtime.clone().into()),
            "TMUX" => Some(tmux.clone().into()),
            _ => None,
        };
        // $TMUX only counts once its socket is the user's.
        assert!(!denied(&env, &host).unwrap().contains(&attached));
        let _attached = UnixListener::bind(&attached).unwrap();
        let paths = denied(&env, &host).unwrap();
        assert!(paths.contains(&socket), "{paths:?}");
        assert!(paths.contains(&attached), "{paths:?}");
        // tmux's whole dir, for the servers it starts there later.
        assert!(paths.contains(&elsewhere), "{paths:?}");
        assert!(!paths.contains(&elsewhere.join("custom")), "{paths:?}");
        assert!(!paths.contains(&dir.join("gone.sock")), "{paths:?}");
        // One under a denied dir isn't a path of its own: the dir is masked
        // read-only first.
        assert!(paths.contains(&runtime), "{paths:?}");
        assert!(!paths.contains(&runtime.join("bus")), "{paths:?}");
        // Another user's sockets aren't the user's doors.
        let other = Host {
            sockets: "/proc/net/unix".into(),
            ..host_in(&dir.join("other"), uid.wrapping_add(1))
        };
        assert!(!denied(&env, &other).unwrap().contains(&socket));
        // And all of them reach the profile.
        let project = dir.join("plain");
        std::fs::create_dir_all(&project).unwrap();
        let cfg = job_in(&dir, &project);
        let job = Job::new(&cfg, "job");
        let config = flags(&cfg, &job, &dir.join("scratch"), &env, &host)
            .unwrap()
            .join("\n");
        for path in [
            &socket,
            &attached,
            &elsewhere,
            &runtime,
            &host.tmp.join(format!("tmux-{uid}")),
        ] {
            assert!(config.contains(&deny(path)), "{path:?}: {config}");
        }
    }

    #[test]
    fn no_socket_table_no_job() {
        let dir = tempfile::tempdir().unwrap();
        let host = Host {
            sockets: dir.path().join("missing"),
            ..host_in(dir.path(), 4242)
        };
        assert!(denied(&env_of(&[("HOME", &dir.path().join("home"))]), &host).is_err());
    }
}
