//! The reviewers' service, run as the steward's machine runs it, with codex
//! and bubblewrap stood in for (tests/fakes): they need bash and jq on PATH.

mod deploy;
mod look;
mod order;
mod out;
mod pause;
mod queue;
mod rig;
mod sessions;
