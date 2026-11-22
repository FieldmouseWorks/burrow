//! The MCP servers behind a plugin's `mcp` grant: of the machine's registry
//! (`burrow.mcpServers`), those the owner approved for its engine
//! (`burrow.agents.engines.NAME.mcpServers`), less any that acts as the
//! owner. docs/engines.md, "A plugin's grants", states the rule.

use crate::{Refusal, Why};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[cfg(test)]
mod tests;

pub(crate) const ACTS_AS_OWNER: &str = "the server acts as the owner \
     (burrow.mcpServers.NAME.actsAsOwner), and no plugin does";

/// The machine's servers, as a plugin's grants are chosen from. Nothing
/// outside this module reads one, so the only way to a server is a grant.
/// An entry has no `envVars` and no host paths: those are a host job's.
#[derive(Debug, Default, Deserialize)]
#[serde(transparent)]
pub struct Registry(BTreeMap<String, Registered>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Registered {
    command: String,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    acts_as_owner: bool,
}

/// A server a plugin was granted. `env` is what the registry sets, and
/// nothing from the environment of whatever runs the plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

impl Registry {
    /// The servers granted, and each one refused that was approved or that
    /// `mcp` would otherwise have brought.
    pub(crate) fn select(
        &self,
        holds_mcp: bool,
        approved: &[String],
    ) -> (BTreeMap<String, Server>, Vec<Refusal>) {
        let mut names: BTreeSet<&String> = approved.iter().collect();
        if holds_mcp {
            names.extend(self.0.keys());
        }
        let mut servers = BTreeMap::new();
        let mut refusals = Vec::new();
        for name in names {
            let why = match self.0.get(name) {
                None => Why::UnknownServer,
                Some(server) if server.acts_as_owner => Why::Never(ACTS_AS_OWNER),
                Some(_) if !holds_mcp => Why::McpNotGranted,
                Some(_) if !approved.contains(name) => Why::ServerNotApproved,
                Some(server) => {
                    servers.insert(
                        name.clone(),
                        Server {
                            command: server.command.clone(),
                            args: server.args.clone(),
                            env: server.env.clone(),
                        },
                    );
                    continue;
                }
            };
            refusals.push(Refusal {
                capability: format!("mcp:{name}"),
                why,
            });
        }
        (servers, refusals)
    }
}
