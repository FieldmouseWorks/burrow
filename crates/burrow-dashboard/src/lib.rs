//! The machine's dashboard (modules/dashboard.nix): a web app on
//! localhost that `tailscale serve` puts on the machine's ts.net name over
//! HTTPS, so only the tailnet reaches it. One owner signs in with a passkey;
//! a device is enrolled with a one-time code printed on the machine
//! (`burrow-dashboard enroll`), and the owner may enrol as many as they
//! like. Pages are rendered here; one small script (assets/app.js) runs the
//! WebAuthn ceremonies, and a service worker (assets/sw.js) caches the shell
//! so the app installs and says when it's offline.
//!
//! - [`core`]: the owner's passkeys, sessions and ceremonies, with no HTTP.
//! - [`session`]: when a session ends.
//! - [`enroll`]: the one-time codes.
//! - [`web`]: the routes, [`pages`]: the HTML, [`assets`]: the shell.

pub mod assets;
pub mod core;
pub mod enroll;
pub mod pages;
pub mod session;
pub mod store;
pub mod web;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

/// What modules/dashboard.nix writes: the machine's own values.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// What the app is called: the machine's name.
    pub title: String,
    /// Where browsers reach it, `https://` and the ts.net name. Its host is
    /// the WebAuthn RP ID, so every passkey is bound to it: it never moves.
    pub origin: String,
    /// The localhost port `tailscale serve` forwards to.
    pub port: u16,
    /// Where the passkeys, sessions and the pending code live, the
    /// service's own and no one else's.
    pub state: PathBuf,
    /// The shell: this crate's assets/.
    pub shell: PathBuf,
    /// The fonts the shell serves (modules/dashboard/fonts.nix).
    pub fonts: PathBuf,
    pub owner: Owner,
    /// Seconds a session lasts unused.
    pub idle_timeout: u64,
    /// Seconds a session lasts at most, used or not.
    pub max_age: u64,
}

/// The one account that may sign in: a GitHub account, by its id (which
/// never changes) and its login (which names it).
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub id: u64,
    pub login: String,
}

impl Config {
    pub fn read(path: &Path) -> Result<Config, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let cfg: Config =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        cfg.rp_id()?;
        Ok(cfg)
    }

    /// The origin's host: the WebAuthn RP ID.
    pub fn rp_id(&self) -> Result<&str, String> {
        let rest = self
            .origin
            .strip_prefix("https://")
            .or_else(|| self.origin.strip_prefix("http://"))
            .ok_or_else(|| format!("origin {} isn't http(s)://HOST", self.origin))?;
        let host = rest.split(':').next().unwrap_or_default();
        if host.is_empty() || rest.contains('/') {
            return Err(format!(
                "origin {} isn't http(s)://HOST[:PORT]",
                self.origin
            ));
        }
        Ok(host)
    }
}

/// Seconds since the epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// N random bytes, from the kernel.
pub fn random<const N: usize>() -> [u8; N] {
    let mut buf = [0; N];
    getrandom::fill(&mut buf).expect("the kernel's random source");
    buf
}

/// A random token: 32 bytes, base64url.
pub fn token() -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random::<32>())
}

/// SHA-256 of a secret, hex: what's kept in place of the secret.
pub fn digest(secret: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Equal, in time that doesn't depend on where they differ.
pub fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(origin: &str) -> Config {
        Config {
            title: "box".into(),
            origin: origin.into(),
            port: 1,
            state: "/nowhere".into(),
            shell: "/nowhere".into(),
            fonts: "/nowhere".into(),
            owner: Owner {
                id: 1,
                login: "sam".into(),
            },
            idle_timeout: 1,
            max_age: 1,
        }
    }

    #[test]
    fn the_rp_id_is_the_origins_host() {
        assert_eq!(
            cfg("https://box.tail1.ts.net").rp_id(),
            Ok("box.tail1.ts.net")
        );
        assert_eq!(cfg("http://localhost:8080").rp_id(), Ok("localhost"));
        assert!(cfg("box.tail1.ts.net").rp_id().is_err());
        assert!(cfg("https://box/path").rp_id().is_err());
        assert!(cfg("https://").rp_id().is_err());
    }

    #[test]
    fn same_compares_whole_strings() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "ab"));
    }
}
