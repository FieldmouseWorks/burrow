//! The smallest crate burrow's lib/project.nix checks, so its Rust gate is
//! tested by burrow's own `nix flake check`.

pub fn add(a: u32, b: u32) -> u32 {
    a + b
}

#[cfg(test)]
mod tests {
    #[test]
    fn adds() {
        assert_eq!(super::add(2, 2), 4);
    }

    // The gate runs tests as `cargo test` does on a laptop: the dev profile,
    // with overflow checks and debug assertions on.
    #[test]
    #[should_panic]
    fn overflow_panics_as_in_cargo_test() {
        super::add(std::hint::black_box(u32::MAX), 1);
    }
}
