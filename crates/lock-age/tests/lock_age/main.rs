//! lock-age with stand-ins for cargo, npm, the registries and release-ages:
//! a refresh's too-new releases are taken back or fail it, and nothing else
//! does. git is the real one, on real repos.

mod branch;
mod cargo;
mod config;
mod npm;
mod rig;
