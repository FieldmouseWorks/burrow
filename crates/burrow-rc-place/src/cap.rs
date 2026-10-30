//! A memory cap as burrow.remoteControl.sessionMemoryMax says it: bytes,
//! with K, M, G or T for powers of 1024 the way systemd reads them, or
//! `infinity` for none.

use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Bytes(u64),
    Infinity,
}

impl FromStr for Cap {
    type Err = String;

    fn from_str(s: &str) -> Result<Cap, String> {
        if s == "infinity" {
            return Ok(Cap::Infinity);
        }
        let (digits, shift) = match s.as_bytes().last() {
            Some(b'K') => (&s[..s.len() - 1], 10),
            Some(b'M') => (&s[..s.len() - 1], 20),
            Some(b'G') => (&s[..s.len() - 1], 30),
            Some(b'T') => (&s[..s.len() - 1], 40),
            _ => (s, 0),
        };
        if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return Err(format!("not a memory cap: {s:?}"));
        }
        digits
            .parse::<u64>()
            .ok()
            .and_then(|n| n.checked_mul(1 << shift))
            .map(Cap::Bytes)
            .ok_or_else(|| format!("too big a memory cap: {s:?}"))
    }
}

impl Cap {
    /// As a cgroup's memory.max takes it.
    pub fn memory_max(self) -> String {
        match self {
            Cap::Bytes(n) => n.to_string(),
            Cap::Infinity => "max".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_what_systemd_reads() {
        assert_eq!("8G".parse(), Ok(Cap::Bytes(8 << 30)));
        assert_eq!("128M".parse(), Ok(Cap::Bytes(128 << 20)));
        assert_eq!("64K".parse(), Ok(Cap::Bytes(64 << 10)));
        assert_eq!("2T".parse(), Ok(Cap::Bytes(2 << 40)));
        assert_eq!("4096".parse(), Ok(Cap::Bytes(4096)));
        assert_eq!("infinity".parse(), Ok(Cap::Infinity));
    }

    #[test]
    fn refuses_anything_else() {
        for s in [
            "",
            "G",
            "8g",
            "8 G",
            "-8G",
            "8.5G",
            "35%",
            "max",
            "99999999999T",
        ] {
            assert!(s.parse::<Cap>().is_err(), "{s:?}");
        }
    }

    #[test]
    fn writes_what_the_kernel_reads() {
        assert_eq!(Cap::Bytes(8 << 30).memory_max(), "8589934592");
        assert_eq!(Cap::Infinity.memory_max(), "max");
    }
}
