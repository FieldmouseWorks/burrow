//! house-sync: every repo's copies of the house blocks (the files in
//! burrow's house/, each between its markers in AGENTS.md) kept the same as
//! burrow's, by modules/house.nix's units:
//!
//!   house-sync CONFIG                     one pass over every repo
//!   house-sync CONFIG OWNER/REPO          one over that repo (a push to its
//!                                         default branch)
//!   house-sync CONFIG status [OWNER/REPO] what a pass would do, changing
//!                                         nothing

pub mod blocks;
pub mod config;
pub mod github;
pub mod pass;
