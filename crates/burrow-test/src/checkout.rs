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

/// The calling crate's program `name`, as this checkout built it, for an
/// integration test that runs it: the path to give `Command::new`.
///
/// `env!("CARGO_BIN_EXE_name")` names the copy Cargo links into the target
/// directory, where another checkout that shares it puts its own when it
/// builds, a concurrent run's included. This finds the package's own, built
/// beside the test's executable in this checkout's build directory
/// (.cargo/config.toml), and refuses when the shared copy isn't it.
#[macro_export]
macro_rules! program {
    ($name:literal) => {{
        $crate::manifest_dir!();
        $crate::checkout::built_program(::core::env!(::core::concat!("CARGO_BIN_EXE_", $name)))
    }};
}

/// The program built beside the test's executable that `linked` is a copy
/// of, looked up once a process: `program!`.
#[doc(hidden)]
pub fn built_program(linked: &'static str) -> &'static str {
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    static FOUND: Mutex<BTreeMap<&str, &str>> = Mutex::new(BTreeMap::new());
    let mut found = FOUND.lock().unwrap_or_else(|e| e.into_inner());
    found.entry(linked).or_insert_with(|| {
        let own = find_program(Path::new(linked)).unwrap_or_else(|why| {
            panic!(
                "{linked} isn't the program this test was built with ({why}): another checkout sharing its target directory built its own since, so run the test again, from inside its checkout (.cargo/config.toml says why)"
            )
        });
        String::leak(own.into_os_string().into_string().unwrap())
    })
}

fn find_program(linked: &Path) -> Result<std::path::PathBuf, String> {
    use std::os::unix::fs::MetadataExt;
    let exe = std::env::current_exe().map_err(|e| format!("this test's own path: {e}"))?;
    // Cargo builds each unit of a package in build/PACKAGE/HASH/out/, a
    // program under its crate name, NAME with `_` for `-` (and its own tests
    // as that and -HASH).
    let package = exe
        .ancestors()
        .nth(3)
        .ok_or_else(|| format!("{} isn't in Cargo's build directory", exe.display()))?;
    let name = linked
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("no name")?
        .replace('-', "_");
    let shared = std::fs::metadata(linked).map_err(|e| format!("{e}"))?;
    let mut copies = Vec::new();
    for unit in std::fs::read_dir(package).map_err(|e| format!("{}: {e}", package.display()))? {
        let path = unit
            .map_err(|e| format!("{e}"))?
            .path()
            .join("out")
            .join(&name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if (meta.dev(), meta.ino()) == (shared.dev(), shared.ino()) {
            return Ok(path);
        }
        if meta.len() == shared.len() {
            copies.push(path);
        }
    }
    // Cargo copies where it can't link: across filesystems.
    let bytes = std::fs::read(linked).map_err(|e| format!("{e}"))?;
    copies
        .into_iter()
        .find(|path| std::fs::read(path).is_ok_and(|b| b == bytes))
        .ok_or_else(|| format!("none of {}/*/out/{name} is", package.display()))
}
