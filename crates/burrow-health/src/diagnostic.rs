//! Drain stderr without retaining more than a diagnostic's first 4 KiB.
use std::io::Read;
use std::process::ChildStderr;

use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

pub(super) struct Diagnostic {
    pipe: ChildStderr,
    bytes: Vec<u8>,
}

impl Diagnostic {
    pub(super) fn new(pipe: ChildStderr) -> std::io::Result<Self> {
        fcntl_setfl(&pipe, fcntl_getfl(&pipe)? | OFlags::NONBLOCK)?;
        Ok(Self {
            pipe,
            bytes: Vec::with_capacity(4096),
        })
    }

    pub(super) fn drain(&mut self) {
        let mut buffer = [0; 8192];
        // A continuously writing descendant must not keep us from checking
        // the deadline or returning after the parent exits. Never wait for EOF.
        for _ in 0..32 {
            match self.pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    let keep = n.min(4096 - self.bytes.len());
                    self.bytes.extend_from_slice(&buffer[..keep]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    }

    pub(super) fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).trim().to_owned()
    }
}
