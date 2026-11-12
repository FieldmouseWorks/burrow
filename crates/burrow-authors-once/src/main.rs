//! `burrow-authors-once ROOT`: scan ROOT's modules/ and lib/ for a second
//! source of the authorized authors (see the library). Exits 1, naming each,
//! when there is one.

use std::path::Path;
use std::process::ExitCode;
use std::{env, fs, io};

use burrow_authors_once::{File, scan};

fn collect(root: &Path, dir: &Path, into: &mut Vec<File>) -> io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.path());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, into)?;
        } else if path.extension().is_some_and(|e| e == "nix") {
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .into_owned();
            into.push(File {
                path: rel,
                text: fs::read_to_string(&path)?,
            });
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let Some(root) = env::args().nth(1) else {
        eprintln!("usage: burrow-authors-once ROOT");
        return ExitCode::from(2);
    };
    let root = Path::new(&root);
    let mut files = Vec::new();
    for dir in ["modules", "lib"] {
        if let Err(e) = collect(root, &root.join(dir), &mut files) {
            eprintln!("burrow-authors-once: {dir}: {e}");
            return ExitCode::from(2);
        }
    }
    let found = scan(&files);
    for line in &found {
        eprintln!("{line}");
    }
    if found.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
