//! A crate whose build script needs a native library, so lib/project.nix's
//! native inputs are tested by burrow's own `nix flake check`.

/// The zlib version the build script found.
pub fn zlib_version() -> &'static str {
    env!("ZLIB_VERSION")
}

#[cfg(test)]
mod tests {
    #[test]
    fn found_zlib() {
        assert!(!super::zlib_version().is_empty());
    }
}
