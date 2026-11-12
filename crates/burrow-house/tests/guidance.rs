//! The checks run by checks/guidance.nix, including malformed source blocks
//! (which sync's reader alone doesn't reject) and stale local copies.
use std::fs;
use std::path::Path;

// Exercise the same entry point as the Nix check. This also compiles on
// the old implementation for the gate's negative run.
fn check(root: &Path) -> Result<(), String> {
    let ran = std::process::Command::new(env!("CARGO_BIN_EXE_burrow-house"))
        .arg("guidance")
        .arg(root)
        .output()
        .unwrap();
    if ran.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8(ran.stderr).unwrap())
    }
}

const BLOCK: &str = "<!-- house:rules v1 begin. -->\n## Rules\n<!-- house:rules end -->\n";

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("house")).unwrap();
    fs::create_dir(dir.path().join("docs")).unwrap();
    put(dir.path(), "house/rules.md", BLOCK);
    put(dir.path(), "AGENTS.md", &format!("# Project\n{BLOCK}own\n"));
    put(dir.path(), "CLAUDE.md", "@AGENTS.md\n");
    put(dir.path(), "README.md", "# Project\n");
    dir
}

fn put(root: &Path, path: &str, text: &str) {
    fs::write(root.join(path), text).unwrap();
}

#[test]
fn whole_blocks_and_guides_pass() {
    let dir = fixture();
    check(dir.path()).unwrap();
    // Body changes are fine when the copy follows; no policy on wording.
    let updated = BLOCK.replace("## Rules", "## Rules\nMore guidance");
    put(dir.path(), "house/rules.md", &updated);
    put(dir.path(), "AGENTS.md", &updated);
    check(dir.path()).unwrap();
}

#[test]
fn invalid_blocks_fail_even_when_the_copy_matches() {
    for broken in [
        "## Rules\n",
        "<!-- house:rules v1 begin -->\nno end\n",
        "<!-- house:wrong v1 begin -->\n<!-- house:wrong end -->\n",
        "<!-- house:rules v1 begin -->\n<!-- house:other end -->\n<!-- house:rules end -->\n",
        "before\n<!-- house:rules v1 begin -->\n<!-- house:rules end -->\n",
    ] {
        let dir = fixture();
        put(dir.path(), "house/rules.md", broken);
        put(dir.path(), "AGENTS.md", broken);
        assert!(
            check(dir.path())
                .unwrap_err()
                .contains("invalid house block")
        );
    }
}

#[test]
fn stale_missing_or_duplicate_copies_fail() {
    for agents in [
        "# Project\n".to_owned(),
        BLOCK.replace("## Rules", "old"),
        BLOCK.repeat(2),
    ] {
        let dir = fixture();
        put(dir.path(), "AGENTS.md", &agents);
        assert!(check(dir.path()).is_err(), "{agents}");
    }
    let dir = fixture();
    fs::remove_file(dir.path().join("house/rules.md")).unwrap();
    assert!(check(dir.path()).is_err());
}

#[test]
fn text_errors_and_missing_guides_fail() {
    for (file, text) in [
        ("CLAUDE.md", "wrong import\n"),
        ("README.md", "no final newline"),
        ("docs/guide.md", "windows\r\n"),
        ("docs/guide.md", "<<<<<<< branch\n"),
        ("docs/guide.md", "text\0\n"),
    ] {
        let dir = fixture();
        put(dir.path(), file, text);
        assert!(check(dir.path()).is_err(), "{file}: {text}");
    }
    let dir = fixture();
    fs::remove_file(dir.path().join("README.md")).unwrap();
    assert!(check(dir.path()).is_err());
}

#[test]
fn deleting_a_source_requires_removing_its_copy() {
    let dir = fixture();
    let other = BLOCK.replace("house:rules", "house:prs");
    put(dir.path(), "house/prs.md", &other);
    put(dir.path(), "AGENTS.md", &format!("{BLOCK}{other}"));
    check(dir.path()).unwrap();
    fs::remove_file(dir.path().join("house/rules.md")).unwrap();
    assert!(
        check(dir.path())
            .unwrap_err()
            .contains("without matching source blocks")
    );
    put(dir.path(), "AGENTS.md", &other);
    check(dir.path()).unwrap();
}
