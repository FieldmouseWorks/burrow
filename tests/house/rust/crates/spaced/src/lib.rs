// word.rs is two modules on purpose.
#![allow(clippy::duplicate_mod)]

#[rustfmt::skip]
#[cfg_attr (all(), path = "word.rs")]
pub mod spaced;

#[cfg(test)]
#[allow(dead_code)]
#[path = "word.rs"]
mod word_alias;
