//! Translate sandbox capability grants (`PROJ_CAPABILITIES`, `PROJ_MCP_SERVERS`,
//! and granted service/tool environment variables) into [`command::Grants`].
//!
//! Builds the child environment from an explicit allowlist
//! (`inherit_environment: false`), never reading or forwarding GitHub tokens,
//! never embedding credentials in `--settings`, and always disabling auto-updates
//! and claude.ai connectors.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use burrow_grants::Capability;
use serde::Deserialize;

use crate::command::{DISABLE_CLAUDEAI_MCP_ENV, Grants, McpServer};

/// GitHub credentials that no engine plugin may ever read or forward.
pub const FORBIDDEN_GITHUB_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_TOKEN_SOCKET",
    "GH_ENTERPRISE_TOKEN",
];

/// Baseline environment variables safe to forward into the harness process.
const BASELINE_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "PATH",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LC_ALL",
    "TERM",
    "TZ",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "SSL_CERT_FILE",
    "NIX_SSL_CERT_FILE",
];

/// Wire shape of an entry in `PROJ_MCP_SERVERS` as `burrow-agent` renders it.
#[derive(Deserialize)]
struct WireMcpServer {
    #[serde(rename = "type")]
    _kind: Option<String>,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    env: BTreeMap<String, String>,
}

/// Build [`Grants`] for one sandboxed job run from the plugin's environment
/// and `workdir`.
pub fn from_env(env: &dyn Fn(&str) -> Option<String>, workdir: &Path) -> Result<Grants, String> {
    from_env_with_host(env, workdir, false)
}

/// Build [`Grants`] for one job run. When `host_mode` is false (the default,
/// used by every sandboxed plugin), enforces the strict environment allowlist
/// and never forwards GitHub tokens or host environment inheritance. When
/// `host_mode` is true (host-only `proj` wrappers), preserves the host-resolved
/// login/App environment, path grants, and MCP servers while still keeping
/// Anthropic credentials out of `--settings`.
pub fn from_env_with_host(
    env: &dyn Fn(&str) -> Option<String>,
    workdir: &Path,
    host_mode: bool,
) -> Result<Grants, String> {
    let caps = parse_capabilities(env("PROJ_CAPABILITIES").as_deref(), host_mode)?;
    let has = |cap: Capability| caps.contains(cap.name());

    let mut out_env = BTreeMap::new();
    if !host_mode {
        for &name in BASELINE_ENV {
            if let Some(val) = env(name) {
                out_env.insert(name.to_owned(), val);
            }
        }
    }

    // Record the granted capability list itself so tools inside the turn see
    // the exact capabilities granted to this invocation.
    let canonical_caps: Vec<&str> = caps.iter().map(String::as_str).collect();
    out_env.insert(
        "PROJ_CAPABILITIES".to_owned(),
        serde_json::to_string(&canonical_caps).map_err(|e| e.to_string())?,
    );

    // Disable claude.ai connectors and auto-updates / plugin self-installation.
    out_env.insert(
        DISABLE_CLAUDEAI_MCP_ENV.0.to_owned(),
        DISABLE_CLAUDEAI_MCP_ENV.1.to_owned(),
    );
    out_env.insert("DISABLE_AUTOUPDATER".to_owned(), "1".to_owned());

    let mut copy_if = |cap: Capability, vars: &[&str]| {
        if has(cap) {
            for &name in vars {
                if let Some(val) = env(name).filter(|v| !v.is_empty()) {
                    out_env.insert(name.to_owned(), val);
                }
            }
        }
    };

    copy_if(Capability::JobState, &["PROJ_HOST_JOBS_SOCKET"]);
    copy_if(Capability::Probe, &["PROJ_PROBE_SOCKET"]);
    copy_if(Capability::UnitsClear, &["UNITS_CLEAR_SOCKET"]);
    copy_if(Capability::Heavy, &["HEAVY_SOCKET"]);
    copy_if(Capability::Toolchains, &["CARGO_HOME", "RUSTUP_HOME"]);
    copy_if(Capability::Nix, &["NIX_REMOTE", "NIX_CONFIG"]);
    copy_if(
        Capability::Network,
        &[
            "http_proxy",
            "https_proxy",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "no_proxy",
            "NO_PROXY",
        ],
    );

    // Defense in depth: never let any Anthropic credential into `grants.env`
    // (which is also serialized into `--settings` argv). In sandbox mode, also
    // strip every GitHub token variable.
    if !host_mode {
        for &name in FORBIDDEN_GITHUB_ENV {
            out_env.remove(name);
        }
    }
    for &name in crate::api::API_CREDENTIALS
        .iter()
        .chain(crate::api::LOGIN_CREDENTIALS)
    {
        out_env.remove(name);
    }

    let paths = if host_mode
        && let Some(raw) = env("PROJ_GRANT_PATHS")
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        serde_json::from_str::<Vec<(PathBuf, bool)>>(raw)
            .map_err(|e| format!("invalid PROJ_GRANT_PATHS JSON: {e}"))?
    } else {
        let mut paths = Vec::new();
        if has(Capability::Workspace) && workdir.is_dir() {
            paths.push((workdir.to_path_buf(), true));
        }
        paths
    };

    let servers = if has(Capability::Mcp) {
        parse_mcp_servers(env("PROJ_MCP_SERVERS").as_deref(), host_mode)?
    } else {
        BTreeMap::new()
    };

    Ok(Grants {
        env: out_env,
        inherit_environment: host_mode,
        paths,
        servers,
    })
}

fn parse_capabilities(raw: Option<&str>, host_mode: bool) -> Result<BTreeSet<String>, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(BTreeSet::new());
    };
    let list: Vec<String> =
        serde_json::from_str(raw).map_err(|e| format!("invalid PROJ_CAPABILITIES JSON: {e}"))?;
    // Never accept capabilities that burrow never grants to a sandboxed plugin.
    let set = list
        .into_iter()
        .filter(|c| {
            host_mode || (c != Capability::Github.name() && c != Capability::Environment.name())
        })
        .collect();
    Ok(set)
}

fn parse_mcp_servers(
    raw: Option<&str>,
    host_mode: bool,
) -> Result<BTreeMap<String, McpServer>, String> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let parsed: BTreeMap<String, WireMcpServer> =
        serde_json::from_str(raw).map_err(|e| format!("invalid PROJ_MCP_SERVERS JSON: {e}"))?;
    let mut servers = BTreeMap::new();
    for (name, wire) in parsed {
        if wire.command.trim().is_empty() {
            return Err(format!("PROJ_MCP_SERVERS[{name:?}] has an empty command"));
        }
        let mut server_env = wire.env;
        if !host_mode {
            for &forbidden in FORBIDDEN_GITHUB_ENV {
                server_env.remove(forbidden);
            }
        }
        servers.insert(
            name,
            McpServer {
                command: wire.command,
                args: wire.args,
                env: server_env,
            },
        );
    }
    Ok(servers)
}
