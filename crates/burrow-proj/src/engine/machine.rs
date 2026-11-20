//! The machine a job's engine runs on, as its grants read it
//! ([`super::capabilities`]): its own paths, its resolver config and its
//! /tmp, and the environment the engine starts in.

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;

/// The environment a job's engine starts in, a variable at a time.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// A variable that says something: unset and empty read alike, as
/// systemd and tmux read them.
fn var(env: Env, name: &str) -> Option<PathBuf> {
    env(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// HOME, made absolute. Unset, the job doesn't start, rather than guess.
fn home(env: Env) -> io::Result<PathBuf> {
    std::path::absolute(var(env, "HOME").ok_or_else(|| io::Error::other("HOME isn't set"))?)
}

/// The machine's own paths a job's commands read: its programs, libraries
/// and config, and sysfs. Only root
/// and system services write in these, so no process of the user's can
/// bind a socket there to run something for the job.
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

/// The machine's /tmp.
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

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::Path;

    /// An environment of just these variables.
    pub(crate) fn env_of(vars: &[(&str, &Path)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let vars: HashMap<String, OsString> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_os_str().to_owned()))
            .collect();
        move |name| vars.get(name).cloned()
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
    fn the_job_reads_the_system_its_toolchains_and_gits_config_and_nothing_else() {
        use std::os::unix::net::UnixListener;
        let dir = tempfile::tempdir().unwrap();
        let machine = machine_in(dir.path());
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".config/git")).unwrap();
        std::fs::create_dir_all(home.join(".rustup")).unwrap();
        std::fs::write(home.join(".gitconfig"), "[user]\n").unwrap();
        // A socket where git's XDG config would be: not a file, not read.
        let _server = UnixListener::bind(home.join(".config/git/config")).unwrap();
        let env = env_of(&[("HOME", &home)]);
        let paths = readable(&env, &machine).unwrap();
        assert_eq!(
            paths,
            [
                dir.path().join("system"),
                dir.path().join("resolved/stub").canonicalize().unwrap(),
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
}
