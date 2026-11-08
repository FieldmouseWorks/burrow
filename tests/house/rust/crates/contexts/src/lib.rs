// shared.rs and piece.rs are two modules each on purpose.
#![allow(clippy::duplicate_mod)]

#[path = "relocated.rs"]
pub mod live;
pub mod parts;

#[cfg(test)]
#[allow(dead_code)]
#[path = "parts/main/piece.rs"]
mod piece_alias;
#[cfg(test)]
#[allow(dead_code)]
#[path = "shared.rs"]
mod shared_alias;
