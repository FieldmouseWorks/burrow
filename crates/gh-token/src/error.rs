//! Why no token came back. Unavailable is what asking again later can fix:
//! GitHub or the network not answering, a rate limit, the service down.
//! Anything else stays as wrong the next time. No error carries a token or
//! the key.

use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub struct Error {
    pub unavailable: bool,
    pub what: String,
}

impl Error {
    pub fn refused(what: impl Into<String>) -> Self {
        Self {
            unavailable: false,
            what: what.into(),
        }
    }

    pub fn unavailable(what: impl Into<String>) -> Self {
        Self {
            unavailable: true,
            what: what.into(),
        }
    }

    /// sysexits' EX_TEMPFAIL for unavailable, so a caller can tell the two.
    pub fn code(&self) -> u8 {
        if self.unavailable { 75 } else { 1 }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.what)
    }
}
