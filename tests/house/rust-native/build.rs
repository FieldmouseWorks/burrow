//! Finds zlib with pkg-config, as a crate that links a native library does,
//! so lib/project.nix's `nativeBuildInputs` (pkg-config) and `buildInputs`
//! (zlib) are shown to reach every build: without them this fails.

use std::process::Command;

fn main() {
    let out = Command::new("pkg-config")
        .args(["--modversion", "zlib"])
        .output()
        .expect("pkg-config is on the build's PATH (the project's nativeBuildInputs)");
    assert!(
        out.status.success(),
        "pkg-config finds zlib (the project's buildInputs): {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let version = String::from_utf8(out.stdout).expect("a UTF-8 version");
    println!("cargo:rustc-env=ZLIB_VERSION={}", version.trim());
}
