pub fn halve(a: u32) -> u32 {
    a / 2
}

// Its tests in a file of their own beside it, by a path attribute, so
// checks/house-package-tests-apart.nix can show the package doesn't change
// with them.
#[cfg(test)]
#[path = "halve_tests.rs"]
mod tests;
