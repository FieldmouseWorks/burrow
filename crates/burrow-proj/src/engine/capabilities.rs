//! The job contract, resolved after the login shell and GitHub App environment.
//! Engine adapters render this data; they do not invent additional grants.
use super::sandbox::{self, Env, Machine};
use crate::{
    config::{Config, McpServer},
    job::Job,
    read,
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capability {
    Workspace,
    JobState,
    GitMetadata,
    Toolchains,
    System,
    Github,
    Heavy,
    Nix,
    Kvm,
    Mcp,
    Network,
    Environment,
}

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
    pub fn network(&self) -> bool {
        self.items
            .iter()
            .any(|g| g.capability == Capability::Network)
    }
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
    pub fn codex_flags(&self) -> Vec<String> {
        let mut flags = Vec::new();
        if !self.repository {
            flags.push("--skip-git-repo-check".into());
        }
        for (name, value) in self.environment() {
            flags.extend([
                "-c".into(),
                format!("shell_environment_policy.set.{}={}", name, quote(&value)),
            ]);
        }
        let servers = self
            .servers()
            .into_iter()
            .map(|(name, server)| {
                let env = server
                    .env
                    .iter()
                    .map(|(key, value)| format!("{}={}", quote(key), quote(value)))
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "{}={{command={},args={},env={{{env}}}}}",
                    quote(&name),
                    quote(&server.command),
                    serde_json::to_string(&server.args).expect("args")
                )
            })
            .collect::<Vec<_>>()
            .join(",");
        flags.extend(["-c".into(), format!("mcp_servers={{{servers}}}")]);
        flags
    }
    pub fn claude_flags(&self) -> Vec<String> {
        let mut flags = vec![
            "--settings".into(),
            serde_json::json!({"env":self.environment()}).to_string(),
        ];
        for grant in &self.items {
            for (path, write) in &grant.paths {
                if *write && path.is_dir() {
                    flags.extend(["--add-dir".into(), path.to_string_lossy().into_owned()]);
                }
            }
        }
        let servers: BTreeMap<_, _> = self.servers().into_iter().map(|(name, server)| {
            (name, serde_json::json!({"type":"stdio", "command":server.command, "args":server.args, "env":server.env}))
        }).collect();
        flags.extend([
            "--strict-mcp-config".into(),
            "--mcp-config".into(),
            serde_json::json!({"mcpServers":servers}).to_string(),
        ]);
        flags
    }
}
fn quote(value: &str) -> String {
    serde_json::to_string(value).expect("string")
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
    let project = PathBuf::from(read(&job.path("project")).unwrap_or_default());
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
    let mut system = sandbox::readable(env, machine)?;
    system.extend(cfg.meters.iter().filter(|p| p.is_dir()).cloned());
    let github = var(env, "GH_TOKEN_SOCKET")
        .map(std::path::absolute)
        .transpose()?
        .and_then(|p| p.parent().map(Path::to_owned));
    system.retain(|p| Some(p) != github.as_ref() && p != Path::new("/nix/var/nix/daemon-socket"));
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
    let mcp = items
        .iter_mut()
        .find(|g| g.capability == Capability::Mcp)
        .unwrap();
    mcp.servers = cfg.mcp_servers.clone();
    for server in mcp.servers.values() {
        mcp.paths
            .extend(server.read_paths.iter().cloned().map(|p| (p, false)));
        mcp.paths
            .extend(server.write_paths.iter().cloned().map(|p| (p, true)));
    }
    Ok(Capabilities { items, repository })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn server_configuration_paths_and_environment_have_one_owner() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.path().join("jobs");
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
                read_paths: vec![socket.clone()],
                write_paths: vec![output.clone()],
            },
        );
        let job = Job::new(&cfg, "test");
        std::fs::create_dir_all(&job.dir).unwrap();
        std::fs::write(job.path("project"), project.to_str().unwrap()).unwrap();
        let home = dir.path().join("home");
        let env = |name: &str| (name == "HOME").then(|| home.clone().into_os_string());
        let machine = sandbox::tests::machine_in(dir.path());
        let grants = resolve(&cfg, &job, &dir.path().join("scratch"), &env, &machine).unwrap();
        let claude = grants.claude_flags();
        let config: serde_json::Value = serde_json::from_str(claude.last().unwrap()).unwrap();
        assert_eq!(
            config["mcpServers"]["machine.ledger"]["command"],
            "/bin/server"
        );
        assert_eq!(
            config["mcpServers"]["machine.ledger"]["env"]["SETTING"],
            "value"
        );
        assert!(claude.contains(&"--strict-mcp-config".to_owned()));
        assert!(claude.contains(&output.to_string_lossy().into_owned()));
        let profile = sandbox::flags(&cfg, &job, &dir.path().join("scratch"), &env, &machine)
            .unwrap()
            .flags
            .join("\n");
        assert!(profile.contains(r#"mcp_servers={"machine.ledger"={command="/bin/server""#));
        assert!(profile.contains(r#"env={"SETTING"="value"}"#));
        assert!(profile.contains(&format!("\"{}\"=\"read\"", socket.display())));
        assert!(profile.contains(&format!("\"{}\"=\"write\"", output.display())));
        assert!(profile.contains(r#""/dev/kvm"="write""#));
        assert_eq!(grants.environment()["NIX_REMOTE"], "daemon");
        // Both adapters take their environment from the same resolved contract.
        let settings: serde_json::Value = serde_json::from_str(&claude[1]).unwrap();
        assert_eq!(settings["env"]["NIX_REMOTE"], "daemon");
        assert!(profile.contains("shell_environment_policy.set.PROJ_CAPABILITIES="));
    }
}
