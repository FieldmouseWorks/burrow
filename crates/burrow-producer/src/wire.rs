//! What a plugin and its producer say over the socket: one line of JSON
//! each way. The plugin sends the flow's request as Conary serialized it,
//! and then nothing more, so the producer reads the connection's end as
//! the plugin gone. The producer answers with one [`Reply`].
use conary_core::plugin::Response;
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Read, Write};

/// The most a line may hold: a request, or a reply carrying an answer.
pub(crate) const MOST: u64 = 16 << 20;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Reply {
    /// The work's answer, which the plugin writes as its own.
    Answer(Box<Response>),
    /// Why there's no answer: the plugin fails with it.
    Refused(String),
}

/// One line, at most [`MOST`] bytes, without its newline. None at the
/// connection's end before any byte.
pub(crate) fn line(from: &mut impl BufRead) -> io::Result<Option<String>> {
    let mut line = String::new();
    let n = from.take(MOST + 1).read_line(&mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "a line that's too long, or cut off",
        ));
    }
    line.pop();
    Ok(Some(line))
}

/// `value` as one line.
pub(crate) fn send(to: &mut impl Write, value: &impl Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    to.write_all(&bytes)?;
    to.flush()
}
