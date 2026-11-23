//! What the upstream changed from the pin to its head that the follower's
//! rules don't call inert. It compares the two trees, not each commit: an
//! inert commit on top of one that matters still moves the pin.

use crate::local::Git;
use crate::manifest::Rules;

/// Whether the rules call the whole path inert.
pub fn inert_path(rules: &Rules, path: &str) -> bool {
    rules.inert.iter().any(|i| match i.strip_suffix('/') {
        Some(dir) => path.strip_prefix(dir).is_some_and(|r| r.starts_with('/')),
        None => path == i,
    })
}

/// The file's lines but the ones that start (past their indent) as one of
/// the prefixes does.
fn kept<'a>(text: &'a str, prefixes: &[String]) -> Vec<&'a str> {
    text.lines()
        .filter(|l| {
            !prefixes
                .iter()
                .any(|p| l.trim_start().starts_with(p.as_str()))
        })
        .collect()
}

/// The paths changed from `pin` to `head` that count: every one but those
/// the rules call inert, and the files whose only differences are in lines
/// they call inert.
pub fn changes(git: &Git, rules: &Rules, pin: &str, head: &str) -> Result<Vec<String>, String> {
    let listed = git.run(&["diff", "--no-renames", "--name-only", "-z", pin, head, "--"])?;
    let mut counted = Vec::new();
    for path in listed.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path).into_owned();
        if inert_path(rules, &path) {
            continue;
        }
        if let Some(prefixes) = rules.inert_lines.get(&path) {
            // One that's gone or new on either side counts.
            if let (Some(a), Some(b)) = (git.show(pin, &path)?, git.show(head, &path)?) {
                let (a, b) = (String::from_utf8_lossy(&a), String::from_utf8_lossy(&b));
                if kept(&a, prefixes) == kept(&b, prefixes) {
                    continue;
                }
            }
        }
        counted.push(path);
    }
    Ok(counted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn commit(dir: &Path, files: &[(&str, &str)]) -> String {
        for (path, text) in files {
            let p = dir.join(path);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        sh(dir, &["add", "-A"]);
        sh(dir, &["commit", "-q", "-m", "c"]);
        sh(dir, &["rev-parse", "HEAD"])
    }

    fn flake(home: &str, body: &str) -> String {
        format!(
            "{{\n  inputs.home.url = \"git+https://github.com/sam/home?ref=main&rev={home}\";\n  outputs = {{ home, ... }}: {body};\n}}\n"
        )
    }

    /// The upstream's history in the shapes that matter: a real change, a
    /// commit that only moves its pin on the follower (as Conary's Renovate
    /// PRs do: flake.nix's line, flake.lock, the workflows), and a real
    /// change with such a commit on top.
    struct Up {
        _dir: tempfile::TempDir,
        git: Git,
        start: String,
        real: String,
        pin_only: String,
        flake_change: String,
        pin_on_top: String,
    }

    fn up() -> Up {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        sh(p, &["init", "-q"]);
        let start = commit(
            p,
            &[
                ("flake.nix", &flake("aaa", "home.lib.project {}")),
                ("flake.lock", "{\"home\": \"aaa\"}\n"),
                (
                    ".github/workflows/flake.yml",
                    "uses: sam/home/.github/workflows/flake.yml@aaa\n",
                ),
                ("crates/core/src/lib.rs", "pub fn f() {}\n"),
            ],
        );
        let real = commit(p, &[("crates/core/src/lib.rs", "pub fn f() -> u8 { 1 }\n")]);
        let pin_only = commit(
            p,
            &[
                ("flake.nix", &flake("bbb", "home.lib.project {}")),
                ("flake.lock", "{\"home\": \"bbb\"}\n"),
                (
                    ".github/workflows/flake.yml",
                    "uses: sam/home/.github/workflows/flake.yml@bbb\n",
                ),
            ],
        );
        let flake_change = commit(
            p,
            &[(
                "flake.nix",
                &flake("bbb", "home.lib.project { checks = {}; }"),
            )],
        );
        let pin_on_top = commit(
            p,
            &[
                (
                    "flake.nix",
                    &flake("ccc", "home.lib.project { checks = {}; }"),
                ),
                ("flake.lock", "{\"home\": \"ccc\"}\n"),
            ],
        );
        let git = Git::new(p.to_path_buf());
        Up {
            _dir: dir,
            git,
            start,
            real,
            pin_only,
            flake_change,
            pin_on_top,
        }
    }

    fn rules() -> Rules {
        Rules {
            inert: vec![".github/".into(), "flake.lock".into()],
            inert_lines: [("flake.nix".into(), vec!["inputs.home.url =".into()])].into(),
        }
    }

    #[test]
    fn a_commit_that_only_moves_the_followers_pin_changes_nothing() {
        let u = up();
        assert_eq!(
            changes(&u.git, &rules(), &u.real, &u.pin_only).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_real_change_counts_with_or_without_a_pin_commit_on_top() {
        let u = up();
        let crate_only = vec!["crates/core/src/lib.rs".to_string()];
        assert_eq!(
            changes(&u.git, &rules(), &u.start, &u.real).unwrap(),
            crate_only
        );
        assert_eq!(
            changes(&u.git, &rules(), &u.start, &u.pin_only).unwrap(),
            crate_only
        );
        // A change to flake.nix's other lines counts, its pin's line on top or not.
        let flake_only = vec!["flake.nix".to_string()];
        assert_eq!(
            changes(&u.git, &rules(), &u.pin_only, &u.flake_change).unwrap(),
            flake_only
        );
        assert_eq!(
            changes(&u.git, &rules(), &u.pin_only, &u.pin_on_top).unwrap(),
            flake_only
        );
        assert_eq!(
            changes(&u.git, &rules(), &u.flake_change, &u.pin_on_top).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn without_the_rules_every_path_counts() {
        // The proof the rules are what let a pin-only commit through: with
        // flake.nix's line not inert, the same commit counts.
        let u = up();
        let mut no_lines = rules();
        no_lines.inert_lines.clear();
        assert_eq!(
            changes(&u.git, &no_lines, &u.real, &u.pin_only).unwrap(),
            vec!["flake.nix".to_string()]
        );
        let got = changes(&u.git, &Rules::default(), &u.real, &u.pin_only).unwrap();
        assert_eq!(
            got,
            vec![".github/workflows/flake.yml", "flake.lock", "flake.nix"]
        );
    }

    #[test]
    fn a_directory_is_inert_only_below_it() {
        let r = rules();
        assert!(inert_path(&r, ".github/workflows/flake.yml"));
        assert!(!inert_path(&r, ".github"));
        assert!(!inert_path(&r, ".githubx/a"));
        assert!(inert_path(&r, "flake.lock"));
        assert!(!inert_path(&r, "sub/flake.lock"));
    }
}
