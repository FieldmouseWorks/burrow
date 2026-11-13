//! A Codex job's sandbox: the pinned CLI's named permissions profile, made
//! by `proj _codex` once the login shell and the App's env have set the
//! environment Codex itself starts in, so the paths it names are the ones
//! Codex and the job's commands find.
//!
//! The shared [`super::capabilities`] contract owns all grants. This adapter
//! renders them into a profile with no `:root`: each command's mounts start
//! from an empty root. Its /tmp is private ([`private_tmp`]), retaining only
//! declared paths the engine or job uses ([`Sandbox::kept`]). Unix socket
//! reach follows the granted paths: the CLI's network switch also covers
//! unix sockets, with no finer seccomp switch. Commands keep no_new_privs,
//! since the filesystem isn't all writable, so sudo can't elevate.

use crate::{config::Config, job::Job};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
#[cfg(test)]
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
/// and config, and sysfs. Only root
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
/// own paths that are there, the resolver's file,
/// rustup's toolchains (RUSTUP_HOME, else HOME's `.rustup`)
/// and git's global config file (GIT_CONFIG_GLOBAL, else HOME's and XDG's),
/// as a regular file, which no socket can be. Other capabilities add their
/// own paths. This base grant excludes broad home and runtime directories:
/// they expose user servers (tmux, systemd, D-Bus and the compile cache),
/// keys, logins, other projects and other jobs.
pub(super) fn readable(env: Env, machine: &Machine) -> io::Result<Vec<PathBuf>> {
    let mut paths: Vec<PathBuf> = machine
        .system
        .iter()
        .filter(|p| p.exists())
        .cloned()
        .collect();
    paths.extend(machine.resolver.canonicalize().ok().filter(|p| p.is_file()));
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

/// Where resolving PATH looks in the machine's TMP, the way the kernel walks
/// it: each name of TMP it looks up, with the rest of the path after it
/// (`/tmp/link/rest`, where a symlink led there), and where it ends. The job's
/// /tmp is another dir, so each of those lookups has to find its path kept
/// there, or PATH would lead somewhere else, or nowhere, in the job: a
/// symlink in /tmp that leads out, one outside that leads in, and every hop
/// of a chain between them. TMP is canonical, as the machine's /tmp is.
fn lookups_in(path: &Path, tmp: &Path) -> io::Result<(Vec<PathBuf>, PathBuf)> {
    use std::collections::VecDeque;
    let named = path.to_owned();
    let parts = |path: &Path| -> VecDeque<OsString> {
        path.components()
            .map(|c| c.as_os_str().to_owned())
            .collect()
    };
    let mut rest = parts(&std::path::absolute(path)?);
    let mut at = PathBuf::from("/");
    let mut lookups = Vec::new();
    let mut links = 0;
    while let Some(part) = rest.pop_front() {
        match part.to_str() {
            Some("/") => at = "/".into(),
            Some(".") => {}
            Some("..") => {
                at.pop();
            }
            _ => {
                if at == tmp {
                    // Only names follow, or the lookup's path would say
                    // something other than where it leads.
                    if rest.iter().any(|p| p == "..") {
                        return Err(io::Error::other(format!(
                            "{} goes back up (..) under {}, which the job's own /tmp can't keep",
                            named.display(),
                            tmp.display()
                        )));
                    }
                    let mut lookup = at.join(&part);
                    lookup.extend(rest.iter().filter(|p| *p != "."));
                    lookups.push(lookup);
                }
                let next = at.join(&part);
                match next.symlink_metadata() {
                    Ok(meta) if meta.is_symlink() => {
                        links += 1;
                        if links > 40 {
                            return Err(io::Error::other(format!(
                                "{} has too many symlinks to resolve",
                                named.display()
                            )));
                        }
                        let mut target = parts(&std::fs::read_link(&next)?);
                        target.extend(rest);
                        rest = target;
                    }
                    _ => at = next,
                }
            }
        }
    }
    Ok((lookups, at))
}

/// The machine's paths under its /tmp that the job's sandbox names, as
/// named, at each hop of a symlink there ([`lookups_in`]), and where they
/// lead, each with the access its entry gives: Codex starts over the job's
/// own /tmp, so these are kept there, bound from the machine's, or Codex's
/// home, the project, its git metadata, the job's state and the tools'
/// config there would be gone or other files. Nothing else of the machine's
/// /tmp is. One that isn't there has nothing to keep; /tmp itself can't be
/// kept, as the job's own is there.
fn under_tmp(
    entries: &BTreeMap<PathBuf, Access>,
    tmp: &Path,
) -> io::Result<Vec<(PathBuf, Access)>> {
    // Walked from the machine's root, so compared where /tmp really is,
    // and kept where it's named.
    let real = tmp.canonicalize().unwrap_or_else(|_| tmp.to_owned());
    let mut kept = Vec::new();
    for (path, access) in entries {
        let (lookups, end) = lookups_in(path, &real)?;
        if end == real {
            return Err(io::Error::other(format!(
                "{} is {}, which is the job's own",
                path.display(),
                tmp.display()
            )));
        }
        for lookup in lookups {
            let place = tmp.join(lookup.strip_prefix(&real).map_err(io::Error::other)?);
            if place.exists() && !kept.contains(&(place.clone(), *access)) {
                kept.push((place, *access));
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
/// A CODEX_HOME that was set is set again to where it leads
/// ([`Sandbox::codex_home`]). The bwrap is unprivileged, so Codex and its
/// hooks run in a user namespace of its own, where zfs sees none of the
/// machine's datasets: the guard takes its undo snapshot through the user's
/// service manager there (burrow-guard's `Namespace`).
pub fn wrap(
    tmp: &Path,
    kept: &[PathBuf],
    codex_home: Option<&Path>,
    codex: &Path,
) -> Vec<OsString> {
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
    if let Some(home) = codex_home {
        argv.extend([
            "--setenv".into(),
            "CODEX_HOME".into(),
            home.as_os_str().to_owned(),
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
    /// Where a CODEX_HOME that was set leads ([`codex_home`]), for Codex to
    /// start with in its place: the path it was set to may go by way of the
    /// machine's /tmp (a symlink there, or one hop of a chain), which the
    /// job's own replaces, while where it leads is the home the profile
    /// denies, there outside /tmp or kept in it.
    pub codex_home: Option<PathBuf>,
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
    flags_with(cfg, job, scratch, env, machine, |c| c)
}

/// [`flags`] over the contract as `narrow` leaves it: how an engine that
/// brings no sandbox of its own gets this one, with only what it was granted.
pub fn flags_with(
    cfg: &Config,
    job: &Job,
    scratch: &Path,
    env: Env,
    machine: &Machine,
    narrow: impl FnOnce(super::capabilities::Capabilities) -> super::capabilities::Capabilities,
) -> io::Result<Sandbox> {
    let capabilities = narrow(super::capabilities::resolve(
        cfg, job, scratch, env, machine,
    )?);
    let mut flags = capabilities.codex_flags();
    let mut entries = BTreeMap::new();
    let grant = |entries: &mut BTreeMap<PathBuf, Access>, path: PathBuf, access: Access| {
        let given = entries.entry(path).or_insert(access);
        *given = access.max(*given);
    };
    for item in &capabilities.items {
        for (path, write) in &item.paths {
            if item.capability == super::capabilities::Capability::Workspace && *path == machine.tmp
            {
                continue;
            }
            grant(
                &mut entries,
                path.clone(),
                if *write { Access::Write } else { Access::Read },
            );
        }
    }
    let control = crate::ask::control_root(cfg);
    std::fs::create_dir_all(&control)?;
    for path in denied(env)?
        .into_iter()
        .chain([control.clone(), control.canonicalize()?])
    {
        grant(&mut entries, path, Access::Deny);
    }
    // What's kept in the job's /tmp gets its own entry there, so /tmp's
    // write grant, which is broader, doesn't reach it: read stays read, and
    // Codex's home stays denied.
    let kept = under_tmp(&entries, &machine.tmp)?;
    for (path, access) in &kept {
        grant(&mut entries, path.clone(), *access);
    }
    for item in &capabilities.items {
        for (path, write) in &item.paths {
            if *path == machine.tmp {
                grant(
                    &mut entries,
                    path.clone(),
                    if *write { Access::Write } else { Access::Read },
                );
            }
        }
    }
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
        format!(
            "permissions.{name}.network.enabled={}",
            capabilities.network()
        ),
        // The login shell's App env and git hook config must reach commands.
        format!(
            "shell_environment_policy.inherit=\"{}\"",
            if capabilities.inherits_environment() {
                "all"
            } else {
                "none"
            }
        ),
        format!(
            "shell_environment_policy.ignore_default_excludes={}",
            capabilities.inherits_environment()
        ),
    ] {
        flags.extend(["-c".into(), config]);
    }
    let codex_home = var(env, "CODEX_HOME")
        .map(|_| codex_home(env))
        .transpose()?;
    Ok(Sandbox {
        flags,
        kept,
        codex_home,
    })
}

#[cfg(test)]
pub(crate) mod tests;
