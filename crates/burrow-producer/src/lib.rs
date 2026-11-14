//! What every Conary flow plugin and its producer share (Conary's
//! `docs/m6-plugins.md`, B2): the flow runs a thin plugin, which forwards
//! each work request over a Unix socket to its producer, a long-lived
//! service with its own system user and its own key.
//!
//! - [`plugin`]: the plugin's side. It answers `describe` and Conary's
//!   conformance fixtures itself, forwards everything else, and fails on
//!   any producer error.
//! - [`serve`]: the producer's socket. Only the flow's controller user may
//!   ask (`SO_PEERCRED`), and a request's work lives only as long as its
//!   connection: a closed one stops it and publishes nothing.
//! - [`records`]: what a producer keeps durably, before it publishes, to
//!   finish publication after a crash. Never read back but at startup, so
//!   never an answer to a later request.
//! - [`sign`]: the producer's key, from its systemd credential only, and
//!   the results it signs, each from its record as it's published. Nothing
//!   signs a statement a caller hands it.
//! - [`publish`]: a record's check run, completed once: with its signed
//!   result, or as `cancelled` when its work is abandoned before it's
//!   committed; and startup reconciliation, which does the same for what a
//!   crash left.

pub mod plugin;
pub mod publish;
pub mod records;
pub mod serve;
pub mod sign;
mod wire;

#[cfg(test)]
mod tests;
