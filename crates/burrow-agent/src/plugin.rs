//! An engine plugin, run in a microVM of the shared sandbox.
//!
//! `agent-plugin-ENGINE` is the executable a Conary runner is given. It
//! takes no arguments and speaks the plugin contract on stdio. It only
//! passes the request to that engine's socket ([`client`]). The service
//! behind the socket ([`serve`]) runs the plugin in a VM of its own for
//! each call, and the call lives only as long as its connection.
//!
//! `describe` runs with nothing granted. A job's request runs the plugin's
//! describe that way first, then boots a second VM holding what that
//! describe declares and the owner approved (burrow-grants), and no more.
//! The guest's user has root, so nothing inside the VM is a guard: what
//! isn't granted is never attached to it ([`render`] says what each grant
//! attaches, [`vm`] attaches it).

pub mod client;
pub mod guest;
pub mod jobs;
pub mod render;
pub mod serve;
pub mod vm;
pub mod workspace;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::{BufRead, Read, Write};
use std::path::PathBuf;

/// The longest line either side reads: a request, or an answer.
pub(crate) const MOST: u64 = 32 * 1024 * 1024;
/// The most a request, or a plugin's answer, may be: below [`MOST`] once
/// escaped into its line.
pub(crate) const MOST_TEXT: u64 = 4 * 1024 * 1024;
/// The virtio port that is the plugin's only channel to the host.
pub(crate) const PORT: &str = "agent.plugin";
/// Where the VM reaches its services: one forwarded port each, on this
/// address, counting up from [`SERVICE_PORT`].
pub(crate) const SERVICE_IP: &str = "10.0.2.101";
pub(crate) const SERVICE_PORT: u16 = 7000;

/// What the client hands the service: the runner's request, and where the
/// caller's job listens for what only the host may do.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Header {
    pub request: String,
    pub probe: Option<PathBuf>,
    pub jobs: Option<PathBuf>,
}

/// What comes back, a line each: from the guest to the service, and from
/// the service to the client.
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Frame {
    Ready,
    Log(String),
    Answer(String),
    Failed(String),
}

/// What the host tells the guest to run. It says nothing the guest could
/// widen: what it names is attached to the VM, or isn't there.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Spec {
    pub request: String,
    pub env: BTreeMap<String, String>,
    pub workspace: Option<render::Workspace>,
    /// The sockets to open in the guest, each carried to the forwarded
    /// port of its place in the list.
    pub sockets: Vec<PathBuf>,
    /// Whether the VM has a NIC to wait for.
    pub nic: bool,
}

pub(crate) fn send<T: Serialize>(to: &mut impl Write, value: &T) -> Result<(), String> {
    let mut line = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    line.push(b'\n');
    to.write_all(&line)
        .and_then(|()| to.flush())
        .map_err(|e| e.to_string())
}

/// The next line of `from`, or None when it ended.
pub(crate) fn receive<T: DeserializeOwned>(from: &mut impl BufRead) -> Result<Option<T>, String> {
    let mut line = Vec::new();
    from.take(MOST)
        .read_until(b'\n', &mut line)
        .map_err(|e| e.to_string())?;
    if line.is_empty() {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') {
        return Err("a message was cut short or is too long".to_owned());
    }
    serde_json::from_slice(&line)
        .map(Some)
        .map_err(|e| format!("a message that doesn't read: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_is_one_whole_line() {
        let mut wire = Vec::new();
        send(&mut wire, &Frame::Answer("{\"a\":\n1}".to_owned())).unwrap();
        let mut read = wire.as_slice();
        assert!(matches!(
            receive::<Frame>(&mut read),
            Ok(Some(Frame::Answer(text))) if text == "{\"a\":\n1}"
        ));
        assert!(matches!(receive::<Frame>(&mut read), Ok(None)));
        wire.pop();
        assert!(receive::<Frame>(&mut wire.as_slice()).is_err());
    }
}
