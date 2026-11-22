//! What each grant is inside the VM. Each is a form of its own, made from
//! the machine's options and the one request, and none is the host job's
//! paths or environment (burrow-grants says why).

use std::collections::BTreeMap;
use std::os::unix::fs::FileTypeExt;
use std::path::{Component, Path, PathBuf};

use burrow_grants::{Capability, Grants};
use conary_core::plugin::{Task, fixtures};
use serde::{Deserialize, Serialize};

use super::Header;
use crate::path::project_name;
use crate::{Config, PluginHost};

const PROXY: &str = "http://10.0.2.100:3128";
/// Where a granted service's socket is in the VM.
const SOCKETS: &str = "/run/agent";

/// A job's workspace: the project's clone, at the project's own path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub project: String,
    pub mount: PathBuf,
    pub workdir: PathBuf,
}

/// What the host does for one connection to a service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Does {
    /// Passes it to this socket, connecting as the user, whom the socket's
    /// owner checks.
    Relay(PathBuf),
    /// Passes it to proj's jobs socket if it only reads (jobs.rs).
    Jobs(PathBuf),
    /// Holds a heavy slot for as long as it's open.
    Heavy(PathBuf),
    /// Runs this MCP server on it, as the user, with the registry's
    /// environment and none of anybody else's.
    Server {
        command: String,
        args: Vec<String>,
        env: BTreeMap<String, String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Service {
    pub socket: PathBuf,
    pub does: Does,
}

/// Everything one VM is given.
#[derive(Debug, Default)]
pub(crate) struct Attach {
    /// Its engine's login, and no other's.
    pub credential: Option<PathBuf>,
    pub network: bool,
    pub kvm: bool,
    pub git_metadata: bool,
    pub workspace: Option<Workspace>,
    pub services: Vec<Service>,
    /// Closures its store has besides the guest's own.
    pub closures: Vec<PathBuf>,
    pub env: BTreeMap<String, String>,
    /// What was granted and has nothing behind it in this run, each with
    /// why. It isn't named to the plugin as held.
    pub absent: Vec<(Capability, &'static str)>,
}

impl Attach {
    /// What describe runs with: nothing. No grant exists before it answers.
    pub fn nothing(cfg: &Config) -> Self {
        let mut attach = Self::default();
        let bins = ["/run/wrappers/bin", "/run/current-system/sw/bin"];
        for (name, value) in [
            ("HOME", cfg.home.to_string_lossy().into_owned()),
            ("USER", cfg.user.clone()),
            ("LOGNAME", cfg.user.clone()),
            ("SHELL", "/run/current-system/sw/bin/bash".to_owned()),
            ("PATH", bins.join(":")),
            ("PROJ_CAPABILITIES", "[]".to_owned()),
        ] {
            attach.env.insert(name.to_owned(), value);
        }
        attach
    }

    /// What a job's request runs with.
    pub fn granted(
        cfg: &Config,
        credential: Option<&Path>,
        grants: &Grants,
        header: &Header,
        task: &Task,
    ) -> Result<Self, String> {
        let mut a = Self::nothing(cfg);
        a.credential = credential.map(Path::to_owned);
        let host = &cfg.plugins;
        if grants.holds(Capability::Workspace) {
            a.workspace = workspace(cfg, task)?;
        }
        let mut held = Vec::new();
        for &capability in grants.items() {
            match a.attach(cfg, host, grants, header, capability) {
                Ok(()) => held.push(capability.name()),
                Err(why) => a.absent.push((capability, why)),
            }
        }
        let held = serde_json::to_string(&held).map_err(|e| e.to_string())?;
        a.env.insert("PROJ_CAPABILITIES".to_owned(), held);
        Ok(a)
    }

    /// No wildcard: a new capability says here what it is in a VM.
    fn attach(
        &mut self,
        cfg: &Config,
        host: &PluginHost,
        grants: &Grants,
        header: &Header,
        capability: Capability,
    ) -> Result<(), &'static str> {
        match capability {
            Capability::Workspace => {
                if self.workspace.is_none() {
                    return Err("the request names no project");
                }
            }
            Capability::GitMetadata => {
                if self.workspace.is_none() {
                    return Err("it comes with a workspace, and this run has none");
                }
                self.git_metadata = true;
            }
            // Never the job's directory: what's written there is proj's word
            // on the job. And proj's jobs socket only to read (jobs.rs).
            Capability::JobState => {
                let to = job_socket(&host.job_sockets, header.jobs.as_deref(), "host-jobs-")
                    .ok_or("the caller has no jobs socket of proj's")?;
                let at = self.service("host-jobs", Does::Jobs(to));
                self.env.insert("PROJ_HOST_JOBS_SOCKET".to_owned(), at);
            }
            Capability::Probe => {
                let to = job_socket(&host.job_sockets, header.probe.as_deref(), "probe-")
                    .ok_or("the caller has no probe socket of proj's")?;
                let at = self.service("probe", Does::Relay(to));
                self.env.insert("PROJ_PROBE_SOCKET".to_owned(), at);
            }
            Capability::UnitsClear => {
                let to = host
                    .units_clear
                    .clone()
                    .ok_or("the machine doesn't watch its units")?;
                let at = self.service("units-clear", Does::Relay(to));
                self.env.insert("UNITS_CLEAR_SOCKET".to_owned(), at);
            }
            // The slots are host locks, which a VM can't take: the host
            // takes one and holds it.
            Capability::Heavy => {
                let heavy = host.heavy.clone().ok_or("the machine has no heavy slots")?;
                let at = self.service("heavy", Does::Heavy(heavy));
                self.env.insert("HEAVY_SOCKET".to_owned(), at);
            }
            Capability::System => self.tools(&host.system),
            Capability::Toolchains => {
                self.tools(&host.toolchains);
                for (name, dir) in [("CARGO_HOME", ".cargo"), ("RUSTUP_HOME", ".rustup")] {
                    let dir = cfg.home.join(dir).to_string_lossy().into_owned();
                    self.env.insert(name.to_owned(), dir);
                }
            }
            // A Nix of the VM's own. The host's daemon would fetch (a
            // fixed-output build) around the proxy, for whoever asks.
            Capability::Nix => {
                self.tools(&host.nix);
                let store = cfg.home.join(".nix-root").to_string_lossy().into_owned();
                self.env.insert("NIX_REMOTE".to_owned(), store);
                self.env.insert(
                    "NIX_CONFIG".to_owned(),
                    "experimental-features = nix-command flakes".to_owned(),
                );
            }
            Capability::Kvm => self.kvm = true,
            Capability::Mcp => self.servers(host, grants)?,
            Capability::Network => {
                self.network = true;
                for name in ["http_proxy", "https_proxy", "HTTP_PROXY", "HTTPS_PROXY"] {
                    self.env.insert(name.to_owned(), PROXY.to_owned());
                }
                for name in ["no_proxy", "NO_PROXY"] {
                    self.env
                        .insert(name.to_owned(), "127.0.0.1,localhost".to_owned());
                }
            }
            Capability::Github | Capability::Environment => {
                return Err("no plugin is ever given it");
            }
        }
        Ok(())
    }

    fn servers(&mut self, host: &PluginHost, grants: &Grants) -> Result<(), &'static str> {
        if grants.servers().is_empty() {
            return Err("no MCP server is approved for this engine");
        }
        let mut rendered = serde_json::Map::new();
        for (i, (name, server)) in grants.servers().iter().enumerate() {
            let at = self.service(
                &format!("mcp-{i}"),
                Does::Server {
                    command: server.command.clone(),
                    args: server.args.clone(),
                    env: server.env.clone(),
                },
            );
            rendered.insert(
                name.clone(),
                serde_json::json!({
                    "type": "stdio",
                    "command": host.socat,
                    "args": ["-", format!("UNIX-CONNECT:{at}")],
                }),
            );
        }
        self.env.insert(
            "PROJ_MCP_SERVERS".to_owned(),
            serde_json::Value::Object(rendered).to_string(),
        );
        Ok(())
    }

    /// Adds a service; returns its socket in the VM.
    fn service(&mut self, name: &str, does: Does) -> String {
        let at = format!("{SOCKETS}/{name}.sock");
        self.services.push(Service {
            socket: at.clone().into(),
            does,
        });
        at
    }

    fn tools(&mut self, tools: &crate::Tools) {
        self.closures.push(tools.closure.clone());
        if let Some(path) = self.env.get_mut("PATH") {
            *path += &format!(":{}/bin", tools.root.display());
        }
    }

    pub fn nic(&self) -> bool {
        self.network || !self.services.is_empty()
    }
}

/// The caller's socket of proj's, only where proj makes one: in a job's
/// scratch, by that name, a socket itself. Any other path is nobody's
/// service (a token's socket, say), whatever the caller's environment says.
fn job_socket(root: &Path, given: Option<&Path>, prefix: &str) -> Option<PathBuf> {
    let given = given?;
    let name = given.file_name()?.to_str()?;
    let turn = name.strip_prefix(prefix)?.strip_suffix(".sock")?;
    if turn.is_empty() || !turn.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    let dir = given.parent()?.canonicalize().ok()?;
    if dir.parent()? != root.canonicalize().ok()? {
        return None;
    }
    let at = dir.join(name);
    std::fs::symlink_metadata(&at)
        .ok()?
        .file_type()
        .is_socket()
        .then_some(at)
}

/// The project the request works in. Conary's conformance job names none
/// (its reserved repository, in `.`) and gets no workspace.
fn workspace(cfg: &Config, task: &Task) -> Result<Option<Workspace>, String> {
    let Task::Job { repo, workdir, .. } = task else {
        return Ok(None);
    };
    let reserved = fixtures().into_iter().any(
        |f| matches!(f.task, Task::Job { repo: r, workdir: w, .. } if r == *repo && w == *workdir),
    );
    if reserved {
        return Ok(None);
    }
    let at = Path::new(workdir);
    let outside = || {
        format!(
            "the workdir {workdir:?} isn't in a project under {}",
            cfg.src.display()
        )
    };
    let inside = at.strip_prefix(&cfg.src).map_err(|_| outside())?;
    if !at.is_absolute()
        || !inside
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return Err(outside());
    }
    let project = inside
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .filter(|p| project_name(p) && !p.starts_with('.'))
        .ok_or_else(outside)?;
    let mount = cfg.src.join(project);
    let plain_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
    // What it writes is in a clone, and only a repository can take it back.
    if !plain_dir(&mount) || !plain_dir(&mount.join(".git")) {
        return Err(format!(
            "{} isn't a repository of its own, so a plugin's work there couldn't be brought back",
            mount.display()
        ));
    }
    Ok(Some(Workspace {
        project: project.to_owned(),
        workdir: at.to_owned(),
        mount,
    }))
}

#[cfg(test)]
mod tests;
