//! The job contract, resolved after the login shell and GitHub App environment.
//! Engine adapters render this data; they do not invent additional grants.
use super::machine::{self, Env, Machine};
use crate::{
    config::{Config, McpServer},
    job::Job,
};
pub use burrow_grants::Capability;
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    process::Command,
};

/// A path's bool is write access; sockets are paths too. Environment and
/// server configuration are carried alongside the paths they depend on.
#[derive(Debug, Serialize)]
pub struct Grant {
    pub capability: Capability,
    pub paths: Vec<(PathBuf, bool)>,
    pub env: BTreeMap<String, String>,
    pub inherit_environment: bool,
    pub servers: BTreeMap<String, McpServer>,
}
impl Grant {
    fn new(capability: Capability, paths: Vec<(PathBuf, bool)>) -> Self {
        Self {
            capability,
            paths,
            env: BTreeMap::new(),
            inherit_environment: false,
            servers: BTreeMap::new(),
        }
    }
}
#[derive(Debug, Serialize)]
pub struct Capabilities {
    pub items: Vec<Grant>,
    repository: bool,
}
impl Capabilities {
    pub fn inherits_environment(&self) -> bool {
        self.items.iter().any(|g| g.inherit_environment)
    }
    pub fn environment(&self) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> =
            self.items.iter().flat_map(|g| g.env.clone()).collect();
        env.insert(
            "PROJ_CAPABILITIES".into(),
            serde_json::to_string(&self.items.iter().map(|g| g.capability).collect::<Vec<_>>())
                .expect("capabilities"),
        );
        env
    }
    pub fn servers(&self) -> BTreeMap<String, McpServer> {
        self.items.iter().flat_map(|g| g.servers.clone()).collect()
    }
}
fn var(env: Env, name: &str) -> Option<PathBuf> {
    env(name).filter(|v| !v.is_empty()).map(PathBuf::from)
}

pub fn resolve(
    cfg: &Config,
    job: &Job,
    scratch: &Path,
    env: Env,
    machine: &Machine,
) -> io::Result<Capabilities> {
    let project = PathBuf::from(
        crate::job::Request::load(job)
            .map_err(io::Error::other)?
            .project,
    );
    let git = Command::new(&cfg.tools.git)
        .arg("-C")
        .arg(&project)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()?;
    let repository = git.status.success();
    let metadata = if repository {
        PathBuf::from(String::from_utf8_lossy(&git.stdout).trim())
    } else {
        project.join(".git")
    };
    let home =
        std::path::absolute(var(env, "HOME").ok_or_else(|| io::Error::other("HOME isn't set"))?)?;
    let cargo = std::path::absolute(var(env, "CARGO_HOME").unwrap_or_else(|| home.join(".cargo")))?;
    let cache =
        std::path::absolute(var(env, "XDG_CACHE_HOME").unwrap_or_else(|| home.join(".cache")))?
            .join("nix");
    for root in [&project, scratch, &job.dir, &cargo, &cache] {
        std::fs::create_dir_all(root)?;
    }
    let mut system = machine::readable(env, machine)?;
    system.extend(
        cfg.meters
            .iter()
            .chain(cfg.github_meters.iter())
            .filter(|p| p.is_dir())
            .cloned(),
    );
    let github = var(env, "GH_TOKEN_SOCKET")
        .map(std::path::absolute)
        .transpose()?
        .and_then(|p| p.parent().map(Path::to_owned));
    let mut items = vec![
        Grant::new(
            Capability::Workspace,
            vec![
                (project, true),
                (scratch.to_owned(), true),
                (machine.tmp.clone(), true),
            ],
        ),
        Grant::new(Capability::JobState, vec![(job.dir.clone(), true)]),
        Grant::new(Capability::GitMetadata, vec![(metadata, true)]),
        Grant::new(Capability::Toolchains, vec![(cargo, true)]),
        Grant::new(
            Capability::System,
            system.into_iter().map(|p| (p, false)).collect(),
        ),
        Grant::new(Capability::Probe, vec![]),
        Grant::new(
            Capability::UnitsClear,
            cfg.units_clear_socket
                .iter()
                .map(|p| (p.clone(), false))
                .collect(),
        ),
        Grant::new(
            Capability::Github,
            github.into_iter().map(|p| (p, false)).collect(),
        ),
        Grant::new(
            Capability::Heavy,
            cfg.heavy_dir
                .iter()
                .filter(|p| p.is_dir())
                .map(|p| (p.clone(), false))
                .collect(),
        ),
        Grant::new(
            Capability::Nix,
            vec![(cache, true), ("/nix/var/nix/daemon-socket".into(), false)],
        ),
        Grant::new(Capability::Kvm, vec![("/dev/kvm".into(), true)]),
        Grant::new(Capability::Mcp, vec![]),
        Grant::new(Capability::Network, vec![]),
        Grant::new(Capability::Environment, vec![]),
    ];
    // The host transport lives in Workspace's scratch. Its address belongs
    // to the contract even though it needs no additional filesystem grant.
    if let Some(socket) = var(env, "PROJ_PROBE_SOCKET") {
        items
            .iter_mut()
            .find(|g| g.capability == Capability::Probe)
            .unwrap()
            .env
            .insert(
                "PROJ_PROBE_SOCKET".into(),
                socket.to_string_lossy().into_owned(),
            );
    }
    if let Some(socket) = var(env, "PROJ_HOST_JOBS_SOCKET") {
        items
            .iter_mut()
            .find(|g| g.capability == Capability::JobState)
            .unwrap()
            .env
            .insert(
                "PROJ_HOST_JOBS_SOCKET".into(),
                socket.to_string_lossy().into_owned(),
            );
    }
    if let Some(engine_home) = var(env, "PROJ_ENGINE_HOME") {
        items
            .iter_mut()
            .find(|g| g.capability == Capability::JobState)
            .unwrap()
            .env
            .insert(
                "PROJ_ENGINE_HOME".into(),
                engine_home.to_string_lossy().into_owned(),
            );
    }
    items
        .iter_mut()
        .find(|g| g.capability == Capability::Nix)
        .unwrap()
        .env
        .insert("NIX_REMOTE".into(), "daemon".into());
    // Login PATH, App credentials and lifecycle variables reach commands
    // unchanged; the command builder appends git hooks after resolution.
    items
        .iter_mut()
        .find(|g| g.capability == Capability::Environment)
        .unwrap()
        .inherit_environment = true;
    let mut capabilities = Capabilities { items, repository };
    let environment = capabilities.environment();
    let mcp = capabilities
        .items
        .iter_mut()
        .find(|g| g.capability == Capability::Mcp)
        .unwrap();
    mcp.servers = cfg.mcp_servers.clone();
    for server in mcp.servers.values_mut() {
        for name in &server.env_vars {
            if let Some(value) = environment
                .get(name)
                .cloned()
                .or_else(|| env(name).and_then(|v| v.into_string().ok()))
            {
                server.env.entry(name.clone()).or_insert(value);
            }
        }
        mcp.paths
            .extend(server.read_paths.iter().cloned().map(|p| (p, false)));
        mcp.paths
            .extend(server.write_paths.iter().cloned().map(|p| (p, true)));
    }
    Ok(capabilities)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_configuration_paths_and_environment_have_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
        let clear_socket = dir.path().join("units/clear.sock");
        cfg.units_clear_socket = Some(clear_socket.clone());
        cfg.tools.git = "git".into();
        let project = dir.path().join("project");
        let socket = dir.path().join("server-socket");
        let output = dir.path().join("server-output");
        std::fs::create_dir_all(&output).unwrap();
        cfg.mcp_servers.insert(
            "machine.ledger".into(),
            McpServer {
                command: "/bin/server".into(),
                args: vec!["--stdio".into()],
                env: BTreeMap::from([("SETTING".into(), "value".into())]),
                env_vars: vec!["SETTING".into(), "EXTRA".into(), "NIX_REMOTE".into()],
                read_paths: vec![socket.clone()],
                write_paths: vec![output.clone()],
            },
        );
        let job = Job::new(&cfg, "test");
        std::fs::create_dir_all(&job.dir).unwrap();
        std::fs::write(job.path("project"), project.to_str().unwrap()).unwrap();
        let home = dir.path().join("home");
        let env = |name: &str| match name {
            "HOME" => Some(home.clone().into_os_string()),
            "SETTING" | "EXTRA" => Some("inherited".into()),
            "PROJ_PROBE_SOCKET" => Some(dir.path().join("scratch/probe.sock").into_os_string()),
            "PROJ_HOST_JOBS_SOCKET" => {
                Some(dir.path().join("scratch/host-jobs.sock").into_os_string())
            }
            "PROJ_ENGINE_HOME" => Some(job.dir.join("engine-home").into_os_string()),
            _ => None,
        };
        let machine = machine::tests::machine_in(dir.path());
        let grants = resolve(&cfg, &job, &dir.path().join("scratch"), &env, &machine).unwrap();
        // The host's contract has every item of the list a plugin is granted from.
        let items: Vec<Capability> = grants.items.iter().map(|g| g.capability).collect();
        assert_eq!(items, Capability::ALL);
        let servers = grants.servers();
        let ledger = &servers["machine.ledger"];
        assert_eq!(ledger.command, "/bin/server");
        assert_eq!(ledger.env["SETTING"], "value");
        assert_eq!(ledger.env["EXTRA"], "inherited");
        assert_eq!(ledger.env["NIX_REMOTE"], "daemon");
        let mcp = grants
            .items
            .iter()
            .find(|g| g.capability == Capability::Mcp)
            .unwrap();
        assert!(mcp.paths.contains(&(output.clone(), true)));
        assert!(mcp.paths.contains(&(socket.clone(), false)));
        let clear = grants
            .items
            .iter()
            .find(|g| g.capability == Capability::UnitsClear)
            .unwrap();
        assert_eq!(clear.paths, vec![(clear_socket.clone(), false)]);
        let env_map = grants.environment();
        assert_eq!(env_map["NIX_REMOTE"], "daemon");
        assert_eq!(
            env_map["PROJ_PROBE_SOCKET"],
            dir.path()
                .join("scratch/probe.sock")
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(
            env_map["PROJ_HOST_JOBS_SOCKET"],
            dir.path()
                .join("scratch/host-jobs.sock")
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(
            env_map["PROJ_ENGINE_HOME"],
            job.dir.join("engine-home").to_string_lossy().as_ref()
        );
        assert!(grants.inherits_environment());
    }
}
