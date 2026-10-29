//! flaky: the GitHub side of quarantining flaky tests. The rules, and what a
//! gate does, are burrow's docs/flaky-tests.md.

pub mod actions;
pub mod filing;
pub mod github;
pub mod quarantine;
pub mod report;
pub mod run;
pub mod test;
