//! The executable side: what a Conary runner starts. It holds no grant and
//! decides none. It hands the request to the engine's socket and prints
//! the answer alone on stdout.

use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use super::{Frame, Header, MOST_TEXT, receive, send};
use crate::Agent;

pub fn call(agent: &Agent, engine: &str) -> Result<(), String> {
    let plugin =
        agent.cfg.engine(engine)?.plugin.as_ref().ok_or_else(|| {
            format!("{engine} has no plugin (burrow.agents.engines.{engine}.plugin)")
        })?;
    let mut request = String::new();
    io::stdin()
        .lock()
        .take(MOST_TEXT + 1)
        .read_to_string(&mut request)
        .map_err(|e| format!("the request: {e}"))?;
    if request.len() as u64 > MOST_TEXT {
        return Err(format!("the request is over {MOST_TEXT} bytes"));
    }
    let socket = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    let header = Header {
        request,
        probe: socket("PROJ_PROBE_SOCKET"),
        jobs: socket("PROJ_HOST_JOBS_SOCKET"),
        instructions: burrow_grants::turn::instructions(|name| std::env::var(name).ok()),
    };
    let mut host = UnixStream::connect(&plugin.socket)
        .map_err(|e| format!("{}: {e}", plugin.socket.display()))?;
    // Never half-closed: the service takes the end of its input as the
    // caller gone (killed at its deadline, say) and ends the call.
    send(&mut host, &header)?;
    let mut answers = BufReader::new(&host);
    loop {
        match receive(&mut answers)? {
            Some(Frame::Ready) => {}
            Some(Frame::Log(text)) => {
                let _ = io::stderr().write_all(text.as_bytes());
            }
            Some(Frame::Answer(text)) => {
                let mut out = io::stdout().lock();
                return out
                    .write_all(text.as_bytes())
                    .and_then(|()| out.flush())
                    .map_err(|e| format!("the answer: {e}"));
            }
            Some(Frame::Failed(why)) => return Err(why),
            None => {
                return Err(format!(
                    "the plugin's run ended without an answer; see: journalctl -t agent-plugin-{engine}"
                ));
            }
        }
    }
}
