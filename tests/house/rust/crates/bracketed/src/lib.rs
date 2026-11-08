// word.rs is two modules on purpose.
#![allow(clippy::duplicate_mod)]

#[cfg_attr(all(), rust_analyzer::ignored([]), path = "word.rs")]
pub mod live;

#[cfg(test)]
#[allow(dead_code)]
#[path = "word.rs"]
mod word_alias;
