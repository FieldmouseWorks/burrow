//! A workspace member, which the Rust gate must check as well as the root.
// shared_tests.rs and type.rs are two modules each on purpose
// (lib/project.nix keeps a file any module that isn't #[cfg(test)] reads).
#![allow(clippy::duplicate_mod)]

pub mod halve;
#[path = "shared_tests.rs"]
pub mod shared;
#[cfg(test)]
#[allow(dead_code)]
#[path = "shared_tests.rs"]
mod shared_again;
pub mod sorting;
pub mod r#type;
#[cfg(test)]
#[allow(dead_code)]
#[path = "type.rs"]
mod type_again;
pub mod vm_tests;

pub fn double(a: u32) -> u32 {
    a * 2
}

// Its unit tests in a file of their own, and an integration test beside
// them (tests/), so checks/house-package-tests-apart.nix can show the
// package doesn't change with them.
#[cfg(test)]
mod tests;
