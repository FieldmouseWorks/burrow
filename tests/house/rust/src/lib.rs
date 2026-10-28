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

    // A tool the project asks for (`testInputs`) is on the tests' PATH.
    #[test]
    fn runs_a_tool_the_project_asked_for() {
        let git = std::process::Command::new("git").arg("--version").output();
        assert!(git.is_ok_and(|out| out.status.success()));
    }
}
