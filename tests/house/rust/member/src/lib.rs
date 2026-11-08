//! A workspace member, which the Rust gate must check as well as the root.

pub fn double(a: u32) -> u32 {
    a * 2
}

// Its unit tests in a file of their own, and an integration test beside
// them (tests/), so checks/house-package-tests-apart.nix can show the
// package doesn't change with them.
#[cfg(test)]
mod tests;
