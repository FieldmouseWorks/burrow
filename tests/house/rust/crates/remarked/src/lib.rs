// word.rs is two modules on purpose.
#![allow(clippy::duplicate_mod)]

#[path = "word.rs"] // the file a #[cfg(test)] module reads too
pub mod remarked;

#[cfg(test)]
#[allow(dead_code)]
#[path = "word.rs"]
mod word_alias;
