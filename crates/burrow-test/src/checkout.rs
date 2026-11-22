//! Where a test finds its checkout.

use std::path::Path;

/// The calling crate's directory, for a test that reads the repo. It refuses
/// when Cargo runs the test for a checkout it wasn't built from.
#[macro_export]
macro_rules! manifest_dir {
    () => {
        $crate::checkout::built_here(::core::env!("CARGO_MANIFEST_DIR"))
    };
}

/// `built`, unless Cargo is running it for another directory. Cargo says
/// which when it runs a test, and a binary run by itself (a check's
/// testBinaries) has only the one it was built from.
///
/// The directory Cargo names is no answer by itself: the test's code, the
/// files compiled into it and the programs it runs would still be the other
/// checkout's.
#[doc(hidden)]
pub fn built_here(built: &'static str) -> &'static Path {
    let built = Path::new(built);
    if let Some(running) = std::env::var_os("CARGO_MANIFEST_DIR") {
        let running = Path::new(&running);
        // Either may name it through a link, and the one built from may be gone.
        let real = |dir: &Path| std::fs::canonicalize(dir).ok();
        assert!(
            built == running || real(built).is_some_and(|dir| real(running) == Some(dir)),
            "this test was built from {} and is run for {}: run cargo from inside the checkout it tests (.cargo/config.toml says why)",
            built.display(),
            running.display(),
        );
    }
    built
}
