//! A growing stream, read incrementally: only complete lines are taken, so a
//! record still being written is read whole next time, never half.
use serde_json::Value;
use std::io::{self, Read, Seek, SeekFrom};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tail {
    /// Bytes taken so far: always just after a newline, or the end once
    /// the stream ended.
    pub offset: u64,
}

impl Tail {
    /// The records written since the last read. `ended` says the stream is
    /// complete, so a last line with no newline is taken too. Lines that
    /// aren't JSON are skipped, as a complete replay skips them.
    pub fn read(&mut self, stream: &mut (impl Read + Seek), ended: bool) -> io::Result<Vec<Value>> {
        stream.seek(SeekFrom::Start(self.offset))?;
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes)?;
        let take = if ended {
            bytes.len()
        } else {
            bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1)
        };
        self.offset += take as u64;
        Ok(bytes[..take]
            .split(|b| *b == b'\n')
            .filter_map(|line| serde_json::from_slice(line).ok())
            .collect())
    }
}
