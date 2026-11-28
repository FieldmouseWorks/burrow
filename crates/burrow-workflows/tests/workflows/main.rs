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
//! One test target, all of it under `tests/`: the negative run knows a test
//! by its path, and takes each module to the old code with the declaration
//! that names it (gate/negative/layout.rs holds the crate to this).

mod attestation;
mod baseline_docs;
mod builds;
mod codeql;
mod commit_signatures;
mod degradations;
mod expr;
mod gate;
mod harness;
mod lane;
mod pr_base;
mod reuse;
mod test_executables;
mod timeouts;
mod train;
mod workspace_lints;
