//! The build store's runner-side rules (docs/gate-design.md, stage 1):
//! a run that may write the store (BUILDS, from the caller's Actions
//! variable, never an input) gets [`NIX_CONFIG`], so a flake's own
//! `nixConfig` can't add a substituter, trust a key, or drop the sandbox.
use super::*;

/// What a run that writes the store adds to every nix it runs, last, so it
/// wins: a flake's `nixConfig` is never taken (so it can't add a
/// substituter, or a key to trust, or turn the sandbox off), whatever the
/// runner's own config says, and builds are sandboxed. What such a run
/// built goes to the store under the machine's key.
pub const NIX_CONFIG: &str = "accept-flake-config = false\nsandbox = true";

/// Whether this run may write the store: its caller named one.
pub fn writes() -> bool {
    !var("BUILDS").is_empty()
}

/// A derivation's name: its path's, past the hash and before `.drv`.
pub fn name(drv: &str) -> String {
    let base = drv.rsplit('/').next().unwrap_or(drv);
    let n = base.split_once('-').map_or(base, |(_, n)| n);
    n.trim_end_matches(".drv").to_owned()
}

#[cfg(test)]
mod tests;
