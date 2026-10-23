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
}
