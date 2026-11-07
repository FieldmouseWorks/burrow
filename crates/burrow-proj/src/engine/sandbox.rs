//! A Codex job's sandbox: the pinned CLI's named permissions profile, made
//! by `proj _codex` once the login shell and the App's env have set the
//! environment Codex itself starts in, so the paths it names are the ones
//! Codex and the job's commands find.
//!
//! The project, scratch, the job's own dir and git's common dir (worktrees
//! made in scratch write there; an explicit grant also lifts the automatic
//! .git protection) are writable, and so is /tmp, which is the job's own
//! ([`private_tmp`]) but for the machine's paths under it that Codex or the
//! job uses, which it keeps ([`Sandbox::kept`]). Of the rest, commands see
//! only what [`readable`] lists, read-only: the profile has no `:root`, so
//! the CLI starts each command's mounts from an empty root. The network is
//! on, which is also what lets a command connect() to the gh-token socket
//! under /run/gh-token: the pinned CLI's seccomp has no finer switch for
//! unix sockets, so which sockets a command can reach is decided by which
//! paths it can see, never by a list of sockets to hide. Commands keep
//! no_new_privs, since the filesystem isn't all writable, so sudo can't
//! elevate.

use crate::{config::Config, job::Job, read};
use std::collections::BTreeMap;
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

/// The machine's own paths a job's commands read: its programs, libraries
/// and config, the nix daemon's socket (for builds) and sysfs. Only root
/// and system services write in these, so no process of the user's can
/// bind a socket there to run something for the job. The CLI's own helpers
/// (the one that reads AGENTS.md, for one) mount its platform defaults
/// ([`PLATFORM`]) over these, so none holds one of them below it: bwrap
/// can't mount on `/run/current-system/sw` inside `/run/current-system`,
/// where it's a symlink.
pub const SYSTEM: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr",
    "/lib",
    "/lib64",
    "/etc",
    "/nix/store",
    "/run/current-system/sw",
    "/nix/var/nix/daemon-socket",
    "/sys",
];

/// The pinned CLI's platform default read roots on Linux
/// (`LINUX_PLATFORM_DEFAULT_READ_ROOTS` in linux-sandbox's bwrap.rs).
#[cfg(test)]
const PLATFORM: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr",
    "/etc",
    "/lib",
    "/lib64",
    "/nix/store",
    "/run/current-system/sw",
];

/// Where the job's own /tmp goes, over the machine's.
pub const TMP: &str = "/tmp";

/// What [`readable`] reads of the machine: its own paths, and the resolver
/// config, whose file (systemd-resolved's stub, under /run) is a
/// symlink's target outside them; and its /tmp, which the job's replaces.
pub struct Machine {
    pub system: Vec<PathBuf>,
    pub resolver: PathBuf,
    pub tmp: PathBuf,
}

impl Machine {
    pub fn real() -> Self {
        Self {
            system: SYSTEM.iter().map(PathBuf::from).collect(),
            resolver: "/etc/resolv.conf".into(),
            tmp: TMP.into(),
        }
    }
}

/// What a command may do with a path. Ordered as the pinned CLI settles
/// two entries for the same path: deny over write over read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Access {
    Read,
    Write,
    Deny,
}

impl Access {
    fn name(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Deny => "deny",
        }
    }
}

/// What the job's commands may read, besides their grants: the machine's
/// own paths that are there, the resolver's file, the dir of the App's
/// token socket, rustup's toolchains (RUSTUP_HOME, else HOME's `.rustup`)
/// and git's global config file (GIT_CONFIG_GLOBAL, else HOME's and XDG's),
/// as a regular file, which no socket can be. Nothing else of the machine
/// is mounted in the sandbox, so a server of the user's, wherever it binds
/// its socket and whenever it starts (a tmux, the systemd manager or D-Bus
/// in the runtime dir, a compile-cache daemon in a network namespace of its
/// own), isn't there to connect() to. Nor are the user's keys and logins
/// (ssh, gh, rclone, the browser's profile), other projects or other jobs.
fn readable(env: Env, machine: &Machine) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = machine
        .system
        .iter()
        .filter(|p| p.exists())
        .cloned()
        .collect();
    paths.extend(machine.resolver.canonicalize().ok().filter(|p| p.is_file()));
    if let Some(socket) = var(env, "GH_TOKEN_SOCKET") {
        paths.extend(std::path::absolute(socket)?.parent().map(Path::to_owned));
    }
    let home = home(env)?;
    let rustup = match var(env, "RUSTUP_HOME") {
        Some(dir) => std::path::absolute(dir)?,
        None => home.join(".rustup"),
    };
    paths.extend(Some(rustup).filter(|p| p.is_dir()));
    let configs = match var(env, "GIT_CONFIG_GLOBAL") {
        Some(file) => vec![std::path::absolute(file)?],
        None => {
            let xdg = match var(env, "XDG_CONFIG_HOME") {
                Some(dir) => std::path::absolute(dir)?,
                None => home.join(".config"),
            };
            vec![home.join(".gitconfig"), xdg.join("git/config")]
        }
    };
    paths.extend(configs.into_iter().filter(|p| p.is_file()));
    Ok(paths)
}

/// Codex's home and its login, which the job's commands have no use for:
/// denied as named and where a symlink there leads, in case a grant reaches
/// either (/tmp's does, when Codex's home is kept there). Its config would
/// otherwise let a job change what Codex runs outside the sandbox.
fn denied(env: Env) -> io::Result<Vec<PathBuf>> {
    let home = codex_home(env)?;
    let auth = home.join("auth.json");
    let mut paths = Vec::new();
    for path in [home, auth] {
        paths.extend(path.canonicalize().ok().filter(|p| *p != path));
        paths.push(path);
    }
    Ok(paths)
}

/// The machine's paths under its /tmp that the job's sandbox names, as
/// named and where a symlink leads, each with the access its entry gives:
/// Codex starts over the job's own /tmp, so these are kept there, bound
/// from the machine's, or Codex's home, the project, its git metadata, the
/// job's state and the tools' config there would be gone or other files.
/// Nothing else of the machine's /tmp is. One that isn't there has nothing
/// to keep; /tmp itself can't be kept, as the job's own is there.
fn under_tmp(
    entries: &BTreeMap<PathBuf, Access>,
    tmp: &Path,
) -> io::Result<Vec<(PathBuf, Access)>> {
    let mut kept = Vec::new();
    for (path, access) in entries {
        let canonical = path.canonicalize().ok();
        for place in [Some(path), canonical.as_ref()].into_iter().flatten() {
            if place == tmp {
                return Err(io::Error::other(format!(
                    "{} is {}, which is the job's own",
                    path.display(),
                    tmp.display()
                )));
            }
            if place.starts_with(tmp) && place.exists() {
                kept.push((place.clone(), *access));
            }
        }
    }
    Ok(kept)
}

/// The paths of LIST that no other of LIST holds.
fn outermost(mut list: Vec<PathBuf>) -> Vec<PathBuf> {
    list.sort();
    list.dedup();
    let all = list.clone();
    list.retain(|p| !all.iter().any(|q| q != p && p.starts_with(q)));
    list
}

/// Each path the machine's /tmp keeps in the job's own goes where it is on
/// the machine, through dirs of the job's /tmp: none may be a symlink the
/// job left there, which would put the machine's path somewhere else.
pub fn check_kept(private: &Path, machine: &Machine, kept: &[PathBuf]) -> io::Result<()> {
    for path in kept {
        let below = path.strip_prefix(&machine.tmp).map_err(io::Error::other)?;
        let mut at = private.to_owned();
        for part in below.components() {
            at.push(part);
            match at.symlink_metadata() {
                Ok(meta) if meta.is_symlink() => {
                    return Err(io::Error::other(format!(
                        "{} is a symlink, where the job's /tmp keeps {}",
                        at.display(),
                        path.display()
                    )));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// The job's own /tmp, a dir in its scratch, the user's alone: proj starts
/// Codex in a mount namespace with it over /tmp ([`wrap`]), so the job's
/// commands write their temp files there and never see the machine's /tmp,
/// where tmux, X and Codex's own daemon keep their sockets.
pub fn private_tmp(scratch: &Path) -> io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let tmp = scratch.join("tmp");
    std::fs::create_dir_all(scratch)?;
    match std::fs::DirBuilder::new().mode(0o700).create(&tmp) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(e),
        _ => {}
    }
    if !tmp.symlink_metadata()?.is_dir() {
        return Err(io::Error::other(format!(
            "{} isn't a dir of the job's own",
            tmp.display()
        )));
    }
    Ok(tmp)
}

/// bwrap's argv to run Codex with TMP as its /tmp, and TMPDIR saying so,
/// the machine's paths KEPT there bound over it from the machine's (bwrap
/// takes a bind's source from the machine's own root), and the rest of the
/// machine as it is: Codex's own sandbox for each command then nests in it.
pub fn wrap(tmp: &Path, kept: &[PathBuf], codex: &Path) -> Vec<OsString> {
    let mut argv: Vec<OsString> = ["--dev-bind", "/", "/", "--bind"]
        .iter()
        .map(OsString::from)
        .collect();
    argv.extend([tmp.as_os_str().to_owned(), TMP.into()]);
    for path in kept {
        argv.extend([
            "--bind".into(),
            path.as_os_str().to_owned(),
            path.as_os_str().to_owned(),
        ]);
    }
    argv.extend(
        ["--setenv", "TMPDIR", "/tmp", "--die-with-parent", "--"]
            .iter()
            .map(OsString::from),
    );
    argv.push(codex.as_os_str().to_owned());
    argv
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

/// The job's sandbox.
pub struct Sandbox {
    /// Codex's flags for it, all of them `exec`'s, to go before `resume`.
    pub flags: Vec<String>,
    /// The machine's paths under its /tmp that Codex or the job uses, kept
    /// in the job's own ([`under_tmp`]).
    pub kept: Vec<PathBuf>,
}

/// The job's sandbox. A project that isn't a repository yet gets its own
/// `.git` granted without making it, so the job can `git init` there (the
/// pinned CLI grants a path that doesn't exist yet), and Codex is told to
/// start outside one.
pub fn flags(
    cfg: &Config,
    job: &Job,
    scratch: &Path,
    env: Env,
    machine: &Machine,
) -> io::Result<Sandbox> {
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
    let mut entries = BTreeMap::new();
    let grant = |entries: &mut BTreeMap<PathBuf, Access>, path: PathBuf, access: Access| {
        let given = entries.entry(path).or_insert(access);
        *given = access.max(*given);
    };
    for path in readable(env, machine)? {
        grant(&mut entries, path, Access::Read);
    }
    for path in roots {
        grant(&mut entries, path, Access::Write);
    }
    for path in denied(env)? {
        grant(&mut entries, path, Access::Deny);
    }
    // What's kept in the job's /tmp gets its own entry there, so /tmp's
    // write grant, which is broader, doesn't reach it: read stays read, and
    // Codex's home stays denied.
    let kept = under_tmp(&entries, &machine.tmp)?;
    for (path, access) in &kept {
        grant(&mut entries, path.clone(), *access);
    }
    grant(&mut entries, machine.tmp.clone(), Access::Write);
    // A deny under another the CLI would mount in a mask already made.
    let denies: Vec<PathBuf> = entries
        .iter()
        .filter(|(_, a)| **a == Access::Deny)
        .map(|(p, _)| p.clone())
        .collect();
    let kept = outermost(kept.into_iter().map(|(p, _)| p).collect());
    let quoted = |p: &Path| serde_json::to_string(&p.to_string_lossy()).expect("string");
    let entries = entries
        .iter()
        .filter(|(p, a)| **a != Access::Deny || !denies.iter().any(|d| d != *p && p.starts_with(d)))
        .map(|(p, a)| format!("{}=\"{}\"", quoted(p), a.name()))
        .collect::<Vec<_>>()
        .join(",");
    let name = profile_name()?;
    for config in [
        r#"approval_policy="never""#.to_owned(),
        format!("default_permissions=\"{name}\""),
        format!("permissions.{name}.filesystem={{{entries}}}"),
        format!("permissions.{name}.network.enabled=true"),
        // The login shell's App env and git hook config must reach commands.
        r#"shell_environment_policy.inherit="all""#.to_owned(),
        // The store is read-only in here: only the daemon builds.
        r#"shell_environment_policy.set.NIX_REMOTE="daemon""#.to_owned(),
        "shell_environment_policy.ignore_default_excludes=true".to_owned(),
    ] {
        flags.extend(["-c".into(), config]);
    }
    Ok(Sandbox { flags, kept })
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
        let argv: Vec<String> = wrap(&tmp, &[], Path::new("/bin/codex"))
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
}
