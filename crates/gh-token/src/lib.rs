//! gh-token: GitHub App installation tokens, from a key the askers can't
//! read. The CLI (main.rs) is the service's side (answer.rs) and the
//! askers' (client.rs, remote.rs); a program given the App's key itself
//! mints with app.rs.
//!
//! A token comes back for an account (every repo the App's installation on
//! it reaches) or for one repo of it. It's good for at least fifty minutes,
//! and for an hour at most.

mod account;
pub mod answer;
pub mod app;
pub mod cache;
pub mod client;
mod command;
pub mod error;
#[cfg(any(test, feature = "fake"))]
pub mod fake;
pub mod remote;
pub mod target;
