//! A workspace below its repo's root, whose test reads a file outside it,
//! as lib/project.nix's `include` gives it to the checks.

#[cfg(test)]
mod tests {
    #[test]
    fn reads_a_file_outside_the_workspace() {
        let fact = concat!(env!("CARGO_MANIFEST_DIR"), "/../spec/fact.txt");
        assert_eq!(std::fs::read_to_string(fact).unwrap(), "42\n");
    }
}
