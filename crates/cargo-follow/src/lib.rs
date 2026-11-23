//! cargo-follow: a Cargo dependency pinned by `rev` on another repo, kept at
//! that repo's default branch's head, as one PR in the repo the pin is in.
//!
//! A manifest opts a pin in under `[workspace.metadata.follow."URL"]` (or
//! `[package.metadata.follow."URL"]`), URL being the dependencies' `git`.
//! There it says what in the upstream changes nothing its crates build
//! (manifest.rs): paths (`inert`) and, in a file, lines (`inert-lines`). A
//! pass compares the upstream's tree at the pin with its head (changes.rs):
//! when something else differs, it bumps every dependency on that URL to the
//! head, lock file and all, from one branch, `follow/OWNER-NAME`, refreshed
//! rather than stacked; when nothing does, it opens nothing and closes the
//! PR it had open (pass.rs).
//!
//! That bound is what keeps two repos that pin each other from chasing one
//! another: an upstream commit that only moves its own pin on the follower
//! (its lock, its workflows, its pin's line) is inert, so landing the
//! follower's bump doesn't make the next one.
//!
//! The machine's handler (modules/updates/follow.nix) starts a pass over a
//! repo on each push to its default branch, the follower's or an upstream's,
//! and the full pass when burrow-events starts and hourly.

pub mod changes;
pub mod config;
pub mod local;
pub mod manifest;
pub mod pass;
