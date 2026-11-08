// word.rs is two modules on purpose.
#![allow(clippy::duplicate_mod)]

#[path = r"word.rs"]
pub mod raw;

#[cfg(test)]
#[allow(dead_code)]
#[path = "word.rs"]
mod word_alias;
