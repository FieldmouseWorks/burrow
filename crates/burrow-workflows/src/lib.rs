//! The shared workflows' tests. Each reads a workflow under `.github/` (or
//! the project template's caller) and runs that workflow's own bash steps as
//! a runner runs them, on real git repos, with stand-ins for what reaches
//! GitHub or builds (`gh`, `curl`, `nix build`). The workflow stays the one
//! home of what it does; these hold it to what it says.
//!
//! They read files outside `crates/`, by their place in a checkout:
//! lib/project.nix's `include` (flake.nix names them) puts the same files in
//! the same places for the flake's checks. They run git, jq and nix, which
//! `cargo test` here needs on PATH.
//!
//! Tests only: outside `cargo test` the crate is empty.
#![cfg(test)]

mod attestation;
mod commit_signatures;
mod expr;
mod gate;
mod harness;
mod keep_builds;
mod lane;
mod pr_base;
