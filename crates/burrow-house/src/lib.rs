//! burrow-house: the house rules a machine can read, held to every PR
//! (house/rules.md). burrow's house action runs it on each PR of a repo that
//! calls it (.github/actions/house), on every push and whenever the PR's
//! title or body is edited, and each rule is a commit status of its own,
//! `house/RULE (#N)`:
//!
//!   - title: the PR's title is `area: plain sentence`, the house commit
//!     style. The steward squash-merges, so the title is the subject main
//!     gets.
//!   - rust-first: a PR that adds Python, or grows Python and shell by more
//!     than a fix does, says why it isn't Rust on a line starting
//!     `Why not Rust:`. A new shell script counts its lines, so glue passes.
//!   - timers: work starts from events, so a PR that adds a timer (a systemd
//!     timer, `startAt`, a cron schedule) says why on a `Why a timer:` line.
//!   - file-size: a PR that grows a code file past its limit, or grows one
//!     already past it, splits it or says why not on a `Why not split:`
//!     line.
//!   - model-names: tracked files never name a model, so a PR that adds a
//!     line naming one says why on a `Why a model name:` line. A repo whose
//!     subject is models (wend records which one wrote each fix) turns it
//!     off.
//!
//! Each line goes in the PR's body, starting a line of its own, with a
//! reason after it (rules.rs says what counts as one). The check reads the
//! title and body when it runs, so fixing either runs it again and the new
//! status replaces the old: a commit keeps one status per context, the
//! newest, and the PR's number in it keeps another PR at the same commit
//! from replacing it. check.rs says how runs that overlap leave the last
//! word with the PR as it is.

pub mod check;
pub mod diff;
pub mod github;
pub mod rules;
