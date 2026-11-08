pub fn answer() -> u32 {
    42
}

// Its tests in a file of their own, which members-hello's package leaves
// out (tests/house-members.nix).
#[cfg(test)]
mod tests;
