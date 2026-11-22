//! The job contract's capabilities, and which of them an engine plugin is
//! granted: those its describe declares and its engine's owner-approved list
//! (`burrow.agents.engines.NAME.capabilities`) names, and the MCP servers
//! behind its `mcp` (mcp.rs). docs/engines.md, "A plugin's grants", states
//! the rule.
//!
//! A grant names a capability and carries none of the host's paths or
//! environment. Those burrow-proj resolves for a job on the host can hold
//! credentials (an MCP server's `envVars` are copied from the job's
//! environment), so whoever runs a plugin gives each capability its own form
//! rather than copying the host's.

use conary_core::plugin::{self, Kind};
use serde::{Serialize, Serializer};
use std::{collections::BTreeMap, fmt};

pub mod mcp;
#[cfg(test)]
mod tests;

/// The enum, every variant and each one's name, from one table.
macro_rules! capabilities {
    ($($variant:ident = $name:literal,)*) => {
        /// One item of what a job can be granted.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Capability {
            $($variant,)*
        }

        impl Capability {
            pub const ALL: &[Self] = &[$(Self::$variant,)*];

            /// Its name in the owner's list and in `PROJ_CAPABILITIES`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Self::$variant => $name,)*
                }
            }
        }
    };
}

capabilities! {
    Workspace = "workspace",
    JobState = "job-state",
    GitMetadata = "git-metadata",
    Toolchains = "toolchains",
    System = "system",
    Probe = "probe",
    UnitsClear = "units-clear",
    Github = "github",
    Heavy = "heavy",
    Nix = "nix",
    Kvm = "kvm",
    Mcp = "mcp",
    Network = "network",
    Environment = "environment",
}

impl Serialize for Capability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.name())
    }
}

/// How a describe declares a capability, if any plugin may hold it.
#[derive(Clone, Copy)]
enum Declaration {
    JobKind,
    Network,
    Mcp,
    Device(&'static str),
    Never(&'static str),
}

const NO_TOKEN: &str = "no plugin holds a GitHub token or the App's installation authority; \
     what a job is authorized to do with them goes through host-mediated services";
const NO_SETTINGS: &str = "no plugin writes the settings store or installs plugins";

/// Authority that is no job capability, named so that asking for it is
/// refused with why.
const WITHHELD: &[(&str, &str)] = &[
    ("github-token", NO_TOKEN),
    ("github-installation", NO_TOKEN),
    ("settings-write", NO_SETTINGS),
    ("plugin-install", NO_SETTINGS),
];

impl Capability {
    /// Conary's describe has a field for the network, MCP and devices. The
    /// rest of the job contract comes with serving the `job` kind. No
    /// wildcard: a new capability says here how it is declared.
    fn declaration(self) -> Declaration {
        match self {
            Self::Workspace
            | Self::JobState
            | Self::GitMetadata
            | Self::Toolchains
            | Self::System
            | Self::Probe
            | Self::UnitsClear
            | Self::Heavy
            | Self::Nix => Declaration::JobKind,
            Self::Kvm => Declaration::Device("kvm"),
            Self::Mcp => Declaration::Mcp,
            Self::Network => Declaration::Network,
            Self::Github => Declaration::Never(NO_TOKEN),
            Self::Environment => Declaration::Never(
                "a job's login environment carries the App's and the owner's credentials",
            ),
        }
    }

    fn declared(self, kinds: &[Kind], offered: &plugin::Capabilities) -> bool {
        match self.declaration() {
            Declaration::JobKind => kinds.contains(&Kind::Job),
            Declaration::Network => offered.network,
            Declaration::Mcp => offered.mcp,
            Declaration::Device(device) => offered.devices.iter().any(|d| d == device),
            Declaration::Never(_) => false,
        }
    }
}

/// Why a capability that was declared or approved isn't granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    NotApproved,
    NotDeclared,
    Never(&'static str),
    UnknownName,
    UnknownDevice,
    ServerNotApproved,
    McpNotGranted,
    UnknownServer,
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotApproved => {
                "the plugin declares it and the owner hasn't approved it \
                 (burrow.agents.engines.NAME.capabilities)"
            }
            Self::NotDeclared => "the owner approved it and the plugin doesn't declare it",
            Self::Never(why) => why,
            Self::UnknownName => "the owner's list names it and no capability has that name",
            Self::UnknownDevice => {
                "the plugin declares it as a device and no capability is that device"
            }
            Self::ServerNotApproved => {
                "mcp is granted and the owner hasn't approved this server \
                 (burrow.agents.engines.NAME.mcpServers)"
            }
            Self::McpNotGranted => "the owner approved the server and mcp isn't granted",
            Self::UnknownServer => {
                "the owner's list names it and burrow.mcpServers has no such server"
            }
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub capability: String,
    pub why: Why,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Quoted: a device's name is whatever the plugin wrote.
        write!(f, "{:?} refused: {}", self.capability, self.why)
    }
}

/// What the owner approved for one engine's plugin.
#[derive(Clone, Copy, Debug)]
pub struct Approved<'a> {
    /// `burrow.agents.engines.NAME.capabilities`.
    pub capabilities: &'a [String],
    /// `burrow.agents.engines.NAME.mcpServers`.
    pub mcp_servers: &'a [String],
    /// What those servers' names are looked up in.
    pub registry: &'a mcp::Registry,
}

/// A plugin's effective grants. Only [`Grants::new`] makes one, so holding
/// one means the rule was applied.
#[derive(Debug)]
pub struct Grants {
    items: Vec<Capability>,
    servers: BTreeMap<String, mcp::Server>,
    refusals: Vec<Refusal>,
}

impl Grants {
    /// `kinds` is every kind the plugin serves (conary-core's
    /// `Plugin::kinds`: describe's `kinds`, `v2_kinds` and `v4_kinds`),
    /// `offered` its describe's `capabilities`, and `approved` what the
    /// owner approved for its engine. `offered.sandbox` says where it runs
    /// and grants nothing.
    pub fn new(kinds: &[Kind], offered: &plugin::Capabilities, approved: &Approved<'_>) -> Self {
        let mut items = Vec::new();
        let mut refused = BTreeMap::new();
        for &capability in Capability::ALL {
            let name = capability.name();
            let is_approved = approved.capabilities.iter().any(|a| a == name);
            let asked = is_approved || offered.devices.iter().any(|d| d == name);
            match (
                capability.declaration(),
                capability.declared(kinds, offered),
                is_approved,
            ) {
                (Declaration::Never(why), ..) if asked => {
                    refused.insert(name, Why::Never(why));
                }
                (Declaration::Never(_), ..) | (_, false, false) => {}
                (_, true, true) => items.push(capability),
                (_, true, false) => {
                    refused.insert(name, Why::NotApproved);
                }
                (_, false, true) => {
                    refused.insert(name, Why::NotDeclared);
                }
            }
        }
        let withheld = |name: &str| {
            WITHHELD
                .iter()
                .find(|(withheld, _)| *withheld == name)
                .map(|(_, why)| Why::Never(why))
        };
        for name in approved.capabilities {
            if !Capability::ALL.iter().any(|c| c.name() == name) {
                refused.insert(name, withheld(name).unwrap_or(Why::UnknownName));
            }
        }
        for device in &offered.devices {
            let known = Capability::ALL
                .iter()
                .any(|c| matches!(c.declaration(), Declaration::Device(d) if d == device));
            if !known {
                refused
                    .entry(device)
                    .or_insert_with(|| withheld(device).unwrap_or(Why::UnknownDevice));
            }
        }
        let (servers, mut refusals) = approved
            .registry
            .select(items.contains(&Capability::Mcp), approved.mcp_servers);
        refusals.extend(refused.into_iter().map(|(name, why)| Refusal {
            capability: name.to_owned(),
            why,
        }));
        refusals.sort_by(|a, b| a.capability.cmp(&b.capability));
        Self {
            items,
            servers,
            refusals,
        }
    }

    pub fn items(&self) -> &[Capability] {
        &self.items
    }

    pub fn holds(&self, capability: Capability) -> bool {
        self.items.contains(&capability)
    }

    /// The only MCP servers the plugin reaches, by name.
    pub fn servers(&self) -> &BTreeMap<String, mcp::Server> {
        &self.servers
    }

    /// What was declared or approved and isn't granted, by name (a server's
    /// is `mcp:NAME`), each with why.
    pub fn refusals(&self) -> &[Refusal] {
        &self.refusals
    }
}
