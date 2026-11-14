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
//!   finish publication after a crash. Never an answer to a later request.
//! - [`sign`]: the producer's key, from its systemd credential only, and
//!   typed signing of its own results. Nothing signs a statement a caller
//!   hands it.
//! - [`publish`]: the check run a signed result rides in, completed as
//!   `cancelled` when its work is abandoned.

pub mod plugin;
pub mod publish;
pub mod records;
pub mod serve;
pub mod sign;
mod wire;

#[cfg(test)]
mod tests;
