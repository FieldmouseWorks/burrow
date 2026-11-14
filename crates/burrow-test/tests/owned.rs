//! Every stand-in for a program `burrow_test` owns is `burrow_test`'s: a
//! harness that writes its own (`("sudo", "#!/bin/sh...`) drifts from the
//! others. This scans the workspace's test sources for one.

use std::fs;
use std::path::{Path, PathBuf};

use burrow_test::OWNED;

fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target") {
                sources(&path, found);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

/// The names, in `text`, of an owned program that is followed by the text of
/// a script: `"sudo", "#!/bin/sh...`, `"sudo", r#"#!...`, `b.join("sudo"),
/// format!("#!...` (comments between them allowed).
pub fn defined(text: &str) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for name in OWNED {
        let quoted = format!("\"{name}\"");
        for (at, _) in text.match_indices(&quoted) {
            let mut rest = &text[at + quoted.len()..];
            // Past the end of a `join(...)` and the comma between.
            rest = rest.trim_start_matches([')', ',', ' ', '\n', '\t']);
            while let Some(comment) = rest.strip_prefix("//") {
                rest = comment.split_once('\n').map_or("", |(_, r)| r);
                rest = rest.trim_start();
            }
            for opener in ["&", "format!(", "String::from("] {
                rest = rest.strip_prefix(opener).unwrap_or(rest);
            }
            let rest = rest.strip_prefix('r').unwrap_or(rest);
            let rest = rest.trim_start_matches('#');
            if rest.strip_prefix('"').is_some_and(|r| r.starts_with("#!")) {
                found.push((text[..at].lines().count(), name));
            }
        }
    }
    found
}

/// Where a stand-in for an owned program is defined in the workspace.
fn offenders(crates: &Path) -> Vec<String> {
    let mut files = Vec::new();
    for entry in fs::read_dir(crates).unwrap().flatten() {
        // burrow-test is the owner.
        if entry.path().is_dir() && entry.file_name() != "burrow-test" {
            sources(&entry.path(), &mut files);
        }
    }
    files.sort();
    let mut offenders = Vec::new();
    for file in files {
        let text = fs::read_to_string(&file).unwrap();
        for (line, name) in defined(&text) {
            offenders.push(format!(
                "{}:{line}: defines {name}",
                file.strip_prefix(crates).unwrap().display()
            ));
        }
    }
    offenders
}

/// `BURROW_TEST_SOURCE_ROOT` points the scan at another checkout's `crates/`
/// (the negative control: today's tree, before a harness moved).
fn crates() -> PathBuf {
    std::env::var_os("BURROW_TEST_SOURCE_ROOT").map_or_else(
        || Path::new(env!("CARGO_MANIFEST_DIR")).join(".."),
        PathBuf::from,
    )
}

#[test]
fn no_harness_defines_a_stand_in_burrow_test_owns() {
    let offenders = offenders(&crates());
    assert!(
        offenders.is_empty(),
        "use burrow_test::stand_ins for these, and keep only the harness's own programs:\n{}",
        offenders.join("\n")
    );
}

#[test]
fn the_scan_sees_each_way_a_stand_in_is_written() {
    for text in [
        r###"("sudo", "#!/bin/sh\nexit 0\n"),"###,
        "(\n    \"systemctl\",\n    // why\n    r#\"#!/bin/sh\n\"#,\n)",
        r###"script(b.join("proj"), r#"#!/bin/sh"#);"###,
        r###"("box-notify", format!("#!/bin/sh\n{X}"))"###,
        r###"("journalctl", "#!/bin/sh\ncat\n")"###,
        r###"("systemd-cat", "#!/bin/sh\n")"###,
        r###"("runuser", r"#!/bin/sh")"###,
    ] {
        assert_eq!(defined(text).len(), 1, "{text}");
    }
}

#[test]
fn the_scan_leaves_alone_what_is_not_a_stand_in() {
    for text in [
        r###"let tools = {"sudo": b.join("sudo"), "proj": "/p"};"###,
        r###"("grub-editenv", "#!/bin/sh\n")"###,
        r###"assert_eq!(m.fake("sudo"), "");"###,
        r###"c.arg("sudo").arg("-u");"###,
    ] {
        assert!(defined(text).is_empty(), "{text}");
    }
}
