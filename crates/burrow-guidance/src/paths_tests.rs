use std::fs;
use std::path::Path;

use super::*;

/// A tree with each of `files` (a trailing `/` makes a directory).
fn tree(files: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for f in files {
        let at = dir.path().join(f);
        if f.ends_with('/') {
            fs::create_dir_all(&at).unwrap();
        } else {
            fs::create_dir_all(at.parent().unwrap()).unwrap();
            fs::write(&at, "x\n").unwrap();
        }
    }
    dir
}

fn cited(own: &Path, burrow: &Path, file: &str, text: &str) -> Vec<String> {
    check_text(file, text, &Trees { own, burrow })
        .into_iter()
        .map(|m| m.to_string())
        .collect()
}

/// burrow as it is now: robot.sh retired for its crate.
fn burrow_now() -> tempfile::TempDir {
    tree(&[
        "modules/robot.nix",
        "modules/claude.nix",
        "crates/burrow-robot/src/main.rs",
        "docs/front-door.md",
    ])
}

/// remi's recovery.md before remi#1227, and as it reads after.
#[test]
fn the_retired_robot_script_fails_and_its_crate_passes() {
    let remi = tree(&["modules/burrow.nix", "docs/recovery.md"]);
    let burrow = burrow_now();
    let before = "anywhere with the Robot credentials (`robot` is a plain bash + curl script:\n\
                  copy burrow's `modules/robot/robot.sh`, set `ROBOT_SERVER_IP=46.4.33.93`,";
    assert_eq!(
        cited(remi.path(), burrow.path(), "docs/recovery.md", before),
        ["docs/recovery.md:2: cites modules/robot/robot.sh, which the pinned burrow doesn't have"]
    );
    let after = "anywhere with the Robot credentials (`robot` is burrow's\n\
                 `crates/burrow-robot`: build it, set `ROBOT_SERVER_IP=46.4.33.93`,";
    assert!(cited(remi.path(), burrow.path(), "docs/recovery.md", after).is_empty());
}

/// The same words pass against a burrow that has the path and fail
/// against one that doesn't: the pin decides, not the repo citing it.
#[test]
fn burrows_paths_resolve_against_the_pinned_burrow() {
    let own = tree(&["modules/burrow.nix"]);
    let old = tree(&["modules/robot/robot.sh"]);
    let new = burrow_now();
    let text = "Copy burrow's `modules/robot/robot.sh`.";
    assert!(cited(own.path(), old.path(), "README.md", text).is_empty());
    assert_eq!(
        cited(own.path(), new.path(), "README.md", text),
        ["README.md:1: cites modules/robot/robot.sh, which the pinned burrow doesn't have"]
    );
    // Not attributed: the repo's own, which has no such file either.
    assert_eq!(
        cited(
            own.path(),
            old.path(),
            "README.md",
            "See `modules/robot/robot.sh`."
        ),
        ["README.md:1: cites modules/robot/robot.sh, which this repository doesn't have"]
    );
    // "burrow's (`a`" is burrow's too, and only the word burrow's is.
    assert!(
        cited(
            own.path(),
            new.path(),
            "x.md",
            "burrow's (`modules/claude.nix`)"
        )
        .is_empty()
    );
    assert_eq!(
        cited(
            own.path(),
            new.path(),
            "x.md",
            "myburrow's `modules/claude.nix`"
        )
        .len(),
        1
    );
}

/// A house block is burrow's text wherever it's copied: its paths are
/// burrow's, and the repo's own text after it is the repo's again.
#[test]
fn a_house_block_cites_burrow() {
    let own = tree(&["modules/burrow.nix"]);
    let burrow = burrow_now();
    let text = "<!-- house:review v3 -->\n\
                an R2 `ask` rule in `modules/claude.nix` dropped\n\
                <!-- house:review end -->\n\
                Ours: `modules/claude.nix`.\n";
    assert_eq!(
        cited(own.path(), burrow.path(), "AGENTS.md", text),
        ["AGENTS.md:4: cites modules/claude.nix, which this repository doesn't have"]
    );
}

#[test]
fn markdown_links_resolve_from_their_document() {
    let own = tree(&[
        "README.md",
        "docs/a.md",
        "docs/b.md",
        "docs/img/x.png",
        "lib/project.nix",
    ]);
    let at = own.path();
    for ok in [
        "[b](b.md)",
        "[b](./b.md#a-heading)",
        "[readme](../README.md)",
        "[project](../lib/project.nix)",
        "[root](/lib/project.nix)",
        "![x](img/x.png)",
        "[dir](img/)",
        "[web](https://example.com/missing.md)",
        "[mail](mailto:a@example.com)",
        "[here](#a-heading)",
        "[titled](b.md \"B\")",
        "[spaced](<b.md>)",
        "[ref]: b.md",
        "[`code` text](b.md)",
    ] {
        assert!(cited(at, at, "docs/a.md", ok).is_empty(), "{ok}");
    }
    assert_eq!(
        cited(
            at,
            at,
            "docs/a.md",
            "[r](README.md) and [p](lib/project.nix)\n[gone](../gone.md#x)\n[out](../../x.md)\n\n[ref]: nope.md\n[file](b.md/)"
        ),
        [
            "docs/a.md:1: cites README.md, which this repository doesn't have",
            "docs/a.md:1: cites lib/project.nix, which this repository doesn't have",
            "docs/a.md:2: cites ../gone.md#x, which this repository doesn't have",
            "docs/a.md:3: cites ../../x.md, which this repository doesn't have",
            "docs/a.md:5: cites nope.md, which this repository doesn't have",
            "docs/a.md:6: cites b.md/, which this repository doesn't have",
        ]
    );
}

#[test]
fn backticked_paths_resolve_from_their_repository() {
    let own = tree(&[
        "lib/project.nix",
        "crates/tool/README.md",
        "crates/tool/src/main.rs",
        ".github/workflows/flake.yml",
    ]);
    let at = own.path();
    for ok in [
        "`lib/project.nix`",
        "`./lib/project.nix`",
        "`lib/project.nix:42`",
        "`lib/`",
        "`.github/workflows/flake.yml`",
        // Not the repository's: runtime paths, state, branches, URLs.
        "`/etc/claude-code/CLAUDE.md`",
        "`~/.config/proj/roles`",
        "`$XDG_CACHE_HOME/flaky`",
        "`.slots/.lock`",
        "`settings/overlay.roles.json`",
        "`conary/train`",
        "`https://github.com/o/r/blob/main/gone.md`",
        "`lib/project.nix's include`",
        "`../lib/gone.nix`",
        // No slash: a name, not a path.
        "`gone.nix`",
    ] {
        assert!(cited(at, at, "docs/a.md", ok).is_empty(), "{ok}");
    }
    // A crate's README names its own files from its directory.
    assert!(cited(at, at, "crates/tool/README.md", "`src/main.rs`").is_empty());
    assert_eq!(
        cited(
            at,
            at,
            "docs/a.md",
            "`lib/gone.nix` and `lib/project.nix/`\n`.github/workflows/gone.yml:3`\n``lib/gone2.nix``"
        ),
        [
            "docs/a.md:1: cites lib/gone.nix, which this repository doesn't have",
            "docs/a.md:1: cites lib/project.nix/, which this repository doesn't have",
            "docs/a.md:2: cites .github/workflows/gone.yml:3, which this repository doesn't have",
            "docs/a.md:3: cites lib/gone2.nix, which this repository doesn't have",
        ]
    );
    assert_eq!(
        cited(at, at, "crates/tool/README.md", "`src/gone.rs`"),
        ["crates/tool/README.md:1: cites src/gone.rs, which this repository doesn't have"]
    );
}

#[test]
fn templates_and_code_blocks_are_never_checked() {
    let own = tree(&["checks/a.nix", "crates/a/", "docs/a.md", "modules/a.nix"]);
    let at = own.path();
    for ok in [
        "`checks/NAME.nix`",
        "`crates/CRATE`",
        "`modules/{a,b}.nix`",
        "`docs/*.md`",
        "`modules/.../x.nix`",
        "`.plans/OWNER_REPO-N/tasks.json`",
        "`refs/agents/ID/*`",
        "`ROLE@OWNER/REPO`",
        "[n](NAME.md)",
        "[t](CRATE/README.md)",
        "```\ncat modules/gone.nix `modules/gone.nix` [x](gone.md)\n```",
        "~~~sh\n`modules/gone.nix`\n~~~",
    ] {
        assert!(cited(at, at, "docs/a.md", ok).is_empty(), "{ok}");
    }
    // Capitals a file is named in by convention are no placeholder.
    assert_eq!(
        cited(at, at, "docs/a.md", "`docs/README.md`, [l](LICENSE)"),
        [
            "docs/a.md:1: cites docs/README.md, which this repository doesn't have",
            "docs/a.md:1: cites LICENSE, which this repository doesn't have",
        ]
    );
    // After a fence closes, lines are checked again.
    assert_eq!(
        cited(
            at,
            at,
            "docs/a.md",
            "```\n`modules/gone.nix`\n```\n`modules/gone.nix`"
        )
        .len(),
        1
    );
}

/// The whole tree: every Markdown file, and the non-Markdown files they
/// cite, so deleting a cited script fails the guidance that names it.
#[test]
fn a_tree_fails_when_a_cited_file_goes() {
    let own = tree(&["tools/run.sh", "tools/other.sh", "docs/run.md"]);
    fs::write(
        own.path().join("docs/run.md"),
        "Run `tools/run.sh`; [its source](../tools/run.sh).\n",
    )
    .unwrap();
    let trees = Trees {
        own: own.path(),
        burrow: own.path(),
    };
    assert_eq!(check(&trees).unwrap(), []);
    fs::remove_file(own.path().join("tools/run.sh")).unwrap();
    // Git keeps no empty directory: one whose last file went is gone too,
    // and so is every path in it from what the check can tell is the
    // repository's.
    assert_eq!(
        check(&trees)
            .unwrap()
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>(),
        [
            "docs/run.md:1: cites tools/run.sh, which this repository doesn't have",
            "docs/run.md:1: cites ../tools/run.sh, which this repository doesn't have",
        ]
    );
}

/// A link's destination is all of what CommonMark takes for it: balanced
/// and escaped parentheses, a title, a line break before it, and a
/// reference's definition in a list or a quote, or with its destination on
/// the next line. Escaped brackets and code spans are no link.
#[test]
fn a_links_whole_destination_is_checked() {
    let own = tree(&["docs/a.md", "docs/a(b).md", "docs/b.md"]);
    let at = own.path();
    for ok in [
        "[x](a(b).md)",
        "[x](a\\(b\\).md)",
        "[x](<a(b).md>)",
        "[x](\nb.md\n\"a title\")",
        "[x](b.md 'B')",
        "[ref]:\n  b.md\n  \"B\"",
        "- item\n\n  [ref]: b.md",
        "\\[x](gone.md)",
        "`[x](gone.md)`",
        "`one\n[x](gone.md) two`",
        "``code ` [x](gone.md)``",
    ] {
        assert!(cited(at, at, "docs/a.md", ok).is_empty(), "{ok}");
    }
    assert_eq!(
        cited(
            at,
            at,
            "docs/a.md",
            "[x](\ngone.md)\n\n[ref]:\n  gone2.md\n\n- [y](gone(3).md)\n\n  [ref2]: gone4.md\n\n> [z](gone5.md \"t\")\n>\n> [ref3]: gone6.md"
        ),
        [
            "docs/a.md:1: cites gone.md, which this repository doesn't have",
            "docs/a.md:4: cites gone2.md, which this repository doesn't have",
            "docs/a.md:7: cites gone(3).md, which this repository doesn't have",
            "docs/a.md:9: cites gone4.md, which this repository doesn't have",
            "docs/a.md:11: cites gone5.md, which this repository doesn't have",
            "docs/a.md:13: cites gone6.md, which this repository doesn't have",
        ]
    );
}

/// A fence closes only on a run of its own character at least as long as
/// its opener, with nothing after it: an example fence inside a longer one,
/// or a line that starts like a fence, leaves it open. Fences in lists and
/// quotes are code too.
#[test]
fn a_fence_runs_to_its_own_close() {
    let own = tree(&["modules/a.nix"]);
    let at = own.path();
    for ok in [
        "````md\n```\n`modules/gone.nix`\n```\n[x](gone.md)\n````",
        "~~~~\n~~~\n`modules/gone.nix`\n~~~~",
        "```\n```sh\n`modules/gone.nix`\n```",
        "- item\n\n  ```\n  `modules/gone.nix` [x](gone.md)\n  ```",
        "> ```\n> `modules/gone.nix`\n> ```",
        "    indented `modules/gone.nix` [x](gone.md)",
    ] {
        assert!(cited(at, at, "docs/a.md", ok).is_empty(), "{ok}");
    }
    // Once it does close, the text after it is read again.
    assert_eq!(
        cited(
            at,
            at,
            "docs/a.md",
            "````\n```\n````\n`modules/gone.nix`\n\n- ```\n  x\n  ```\n\n  [y](gone.md)"
        ),
        [
            "docs/a.md:4: cites modules/gone.nix, which this repository doesn't have",
            "docs/a.md:10: cites gone.md, which this repository doesn't have",
        ]
    );
}

/// A link names a file as it's called: spaces (escaped or in `<...>`) and
/// punctuation are part of its name, not a template's marks; only a
/// placeholder word is.
#[test]
fn link_filenames_are_literal() {
    let own = tree(&[
        "docs/a.md",
        "assets/my image.png",
        "docs/a@b.md",
        "docs/c[1].md",
    ]);
    let at = own.path();
    for ok in [
        "![x](../assets/my%20image.png)",
        "![x](<../assets/my image.png>)",
        "[x](a@b.md)",
        "[x](c\\[1\\].md)",
        "[x](a@b.md?plain=1#L3)",
        "[n](NAME.md)",
    ] {
        assert!(cited(at, at, "docs/a.md", ok).is_empty(), "{ok}");
    }
    assert_eq!(
        cited(
            at,
            at,
            "docs/a.md",
            "![x](../assets/missing%20image.png) [y](<../assets/gone file.png>)\n[z](gone@b.md) [w](c$1.md) [v](g{a,b}.md)"
        ),
        [
            "docs/a.md:1: cites ../assets/missing%20image.png, which this repository doesn't have",
            "docs/a.md:1: cites ../assets/gone file.png, which this repository doesn't have",
            "docs/a.md:2: cites gone@b.md, which this repository doesn't have",
            "docs/a.md:2: cites c$1.md, which this repository doesn't have",
            "docs/a.md:2: cites g{a,b}.md, which this repository doesn't have",
        ]
    );
}

/// A target is in its tree only when what it resolves to is: a symlink
/// out of the tree, or a broken one, at the end or on the way, fails; one
/// to elsewhere in the tree passes. The same for burrow's tree.
#[test]
fn symlinks_stay_in_their_tree() {
    use std::os::unix::fs::symlink;
    let outside = tree(&["passwd", "etc/hosts"]);
    let own = tree(&["docs/a.md", "lib/project.nix", "modules/burrow.nix"]);
    let at = own.path();
    symlink(outside.path(), at.join("out")).unwrap();
    symlink(outside.path().join("passwd"), at.join("lib/passwd")).unwrap();
    symlink(at.join("nowhere"), at.join("dead")).unwrap();
    symlink("../lib", at.join("docs/lib")).unwrap();
    symlink("project.nix", at.join("lib/alias.nix")).unwrap();
    for ok in [
        "[x](lib/project.nix)",
        "[x](lib/alias.nix)",
        "`docs/lib/project.nix`",
        "`lib/alias.nix`",
    ] {
        assert!(cited(at, at, "docs/a.md", ok).is_empty(), "{ok}");
    }
    assert_eq!(
        cited(
            at,
            at,
            "docs/a.md",
            "[a](../out/passwd) [b](/lib/passwd) [c](../dead)\n`out/passwd` `lib/passwd` `dead/x` `out/etc/`"
        ),
        [
            "docs/a.md:1: cites ../out/passwd, which this repository doesn't have",
            "docs/a.md:1: cites /lib/passwd, which this repository doesn't have",
            "docs/a.md:1: cites ../dead, which this repository doesn't have",
            "docs/a.md:2: cites out/passwd, which this repository doesn't have",
            "docs/a.md:2: cites lib/passwd, which this repository doesn't have",
            "docs/a.md:2: cites dead/x, which this repository doesn't have",
            "docs/a.md:2: cites out/etc/, which this repository doesn't have",
        ]
    );
    // burrow's tree is held the same way.
    let burrow = burrow_now();
    symlink(outside.path(), burrow.path().join("modules/out")).unwrap();
    assert_eq!(
        cited(
            at,
            burrow.path(),
            "docs/a.md",
            "burrow's `modules/out/passwd`"
        ),
        ["docs/a.md:1: cites modules/out/passwd, which the pinned burrow doesn't have"]
    );
    assert!(
        cited(
            at,
            burrow.path(),
            "docs/a.md",
            "burrow's `modules/claude.nix`"
        )
        .is_empty()
    );
}
