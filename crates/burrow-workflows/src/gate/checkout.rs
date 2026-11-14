//! The checkout the gate reads: one snapshot of it, as nix flake check reads
//! it, its settings, submodules and uncommitted changes included, purely.

use std::fs;
use std::path::Path;

use super::{Gate, Run};
use crate::harness::{read, write};

/// The settings in effect hold in the snapshot, set, included or included
/// if: a mode git is told to ignore leaves the checkout clean.
#[test]
fn repository_settings_hold_in_the_snapshot() {
    let g = Gate::new();
    let user = g.s.join(".gitconfig");
    let filemode = g.s.join("filemode.inc");
    write(&filemode, "[core]\n\tfilemode = false\n");
    for how in ["set", "included", "included if", "the user's, included if"] {
        let d = g.flake(
            r#"{
          checks.${system}.meta = if self ? rev then drv "clean" else throw "seen as dirty";
        }"#,
        );
        match how {
            "set" => {
                g.s.git(&d, &["config", "core.filemode", "false"]);
            }
            "included" => {
                // Relative to the config file that includes it.
                burrow_test::copy_file(&filemode, d.join(".git/filemode.inc"));
                g.s.git(&d, &["config", "include.path", "filemode.inc"]);
            }
            "included if" => {
                let key = format!("includeIf.gitdir:{}/.path", d.display());
                g.s.git(&d, &["config", &key, filemode.to_str().unwrap()]);
            }
            _ => {
                // git init sets the repository's own, which would win.
                g.s.git(&d, &["config", "--unset", "core.filemode"]);
                write(
                    &user,
                    &format!(
                        "[includeIf \"gitdir:{}/\"]\n\tpath = {}\n",
                        d.display(),
                        filemode.display()
                    ),
                );
            }
        }
        let file = d.join("flake.nix");
        burrow_test::executable(&file, fs::read(&file).unwrap());
        let r = g.apart(&d);
        let _ = fs::remove_file(&user);
        assert_eq!(r.code, 0, "{how}: {}", r.out);
        assert!(r.build().contains("-clean.drv^*"), "{how}");
    }
}

/// A repo of one committed file, `file`.
fn sub_repo(g: &Gate, dir: &Path, text: &str) {
    write(&dir.join("file"), text);
    g.s.git(dir, &["init", "-q"]);
    g.s.git(dir, &["add", "file"]);
    g.s.git(dir, &["commit", "-qm", "sub"]);
}

const SUBMODULE_CHECK: &str = r#"{
      checks.${system}.sub = drv (builtins.readFile ./sub/file);
    }"#;

/// A dirty checkout's submodules are in the snapshot as nix reads them,
/// checked out, uncommitted changes and all.
#[test]
fn a_dirty_checkouts_submodules_are_in_the_snapshot() {
    let g = Gate::new();
    let sub = g.dir();
    sub_repo(&g, &sub, "committed");
    let d = g.flake_with(SUBMODULE_CHECK, "inputs.self.submodules = true;");
    g.s.git(
        &d,
        &["submodule", "add", "-q", sub.to_str().unwrap(), "sub"],
    );
    g.s.git(&d, &["commit", "-qm", "submodule"]);
    write(&d.join("sub/file"), "changed");
    let mut flake = read(&d.join("flake.nix"));
    flake.push_str("# changed\n");
    write(&d.join("flake.nix"), &flake);
    let r = g.apart(&d);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.build().contains("-changed.drv^*"));
}

/// A clean checkout's submodule at a relative URL, with no remote, is
/// fetched from beside the checkout, not the snapshot.
#[test]
fn a_relative_submodule_is_fetched_from_beside_the_checkout() {
    let g = Gate::new();
    let parent = g.dir();
    let sub = parent.join("sub");
    fs::create_dir(&sub).unwrap();
    sub_repo(&g, &sub, "beside");
    let d = g.flake_with(SUBMODULE_CHECK, "inputs.self.submodules = true;");
    let top = parent.join("top");
    fs::rename(&d, &top).unwrap();
    g.s.git(&top, &["submodule", "add", "-q", "../sub", "sub"]);
    g.s.git(&top, &["commit", "-qm", "submodule"]);
    let r = g.apart(&top);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.build().contains("-beside.drv^*"));
}

/// An edit to the checkout while the gate runs changes nothing it
/// evaluates, and nothing writes the checkout. A lock file that needs
/// changing fails it, and isn't written.
#[test]
fn the_gate_evaluates_one_snapshot() {
    let g = Gate::new();
    let d = g.flake(r#"{ checks.${system} = { a = drv "a"; b = drv "b"; }; }"#);
    let edit = g.s.join("edited.nix");
    write(
        &edit,
        &format!(
            "{{\n  outputs = {{ self }}: {{\n    checks.{} = {{\n      b = {};\n      c = {};\n    }};\n  }};\n}}\n",
            g.system,
            g.drv("b2"),
            g.drv("c")
        ),
    );
    let flake = d.join("flake.nix");
    let r = g.run(
        &d,
        Run {
            skip: "a",
            env: &[
                ("EDIT", edit.to_str().unwrap()),
                ("EDIT_TO", flake.to_str().unwrap()),
            ],
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(read(&flake), read(&edit), "the edit wasn't made");
    let b = r.build();
    assert!(
        b.contains("-b.drv^*")
            && !b.contains("-a.drv")
            && !b.contains("-b2.drv")
            && !b.contains("-c.drv"),
        "{b}"
    );
    let dep = g.flake("{ }");
    let r = g.apart(&g.flake_with(
        r#"{ checks.${system}.a = drv "a"; }"#,
        &format!(r#"inputs.dep.url = "path:{}";"#, dep.display()),
    ));
    assert!(
        r.fails_with("lock file") && r.builds().is_empty(),
        "{}",
        r.out
    );
}

/// A clean checkout is evaluated with its commit as self's.
#[test]
fn a_clean_checkout_is_its_commit() {
    let g = Gate::new();
    let d = g.flake(
        r#"{
      checks.${system}.meta = if self ? rev && self ? revCount && self.lastModified > 0
        then drv "meta-${self.shortRev}" else throw "lost the commit";
    }"#,
    );
    let rev = g.s.git(&d, &["rev-parse", "--short=7", "HEAD"]);
    let r = g.apart(&d);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.build().contains(&format!("-meta-{rev}.drv^*")), "{rev}");
}

/// gate-reuse: the gate checks a private checkout of the tree, whose commit
/// is the tree's alone, so one tree is checked as one source whatever has
/// it: another commit of it (later, with a parent), a depth-1 clone, a
/// worktree, a checkout whose git config would encode commits otherwise. In
/// both evaluation modes, each builds the derivation the first did, and
/// sees a revCount and lastModified of 1; without gate-reuse their own
/// metadata makes them others. The caller's checkout is left as it was.
#[test]
fn with_gate_reuse_a_tree_is_checked_as_its_tree_alone() {
    let g = Gate::new();
    let d = g.flake(
        r#"{
      checks.${system}.meta = drv "meta-${self.shortRev or "none"}-${toString (self.revCount or 0)}-${toString (self.lastModified or 0)}";
    }"#,
    );
    let first = g.s.git(&d, &["rev-parse", "HEAD"]);
    g.s.git(&d, &["commit", "-q", "--allow-empty", "-m", "again"]);
    let shallow = g.s.join("shallow");
    g.s.git(
        g.s.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &format!("file://{}", d.display()),
            shallow.to_str().unwrap(),
        ],
    );
    assert_eq!(
        g.s.git(&shallow, &["rev-parse", "--is-shallow-repository"]),
        "true"
    );
    let worktree = g.s.join("worktree");
    g.s.git(
        &d,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().unwrap(),
            &first,
        ],
    );
    let encoded = g.s.join("encoded");
    g.s.git(
        g.s.path(),
        &[
            "clone",
            "-q",
            d.to_str().unwrap(),
            encoded.to_str().unwrap(),
        ],
    );
    g.s.git(&encoded, &["config", "i18n.commitEncoding", "ISO-8859-1"]);
    for whole in [false, true] {
        let build = |at: &Path, source: bool| {
            let head = g.s.git(at, &["rev-parse", "HEAD"]);
            let r = g.run(
                at,
                Run {
                    whole,
                    env: &[("TREE_SOURCE", if source { "true" } else { "false" })],
                    ..Run::default()
                },
            );
            assert_eq!(r.code, 0, "{}", r.out);
            // The caller's checkout, as it was.
            assert_eq!(g.s.git(at, &["rev-parse", "HEAD"]), head);
            assert_eq!(g.s.git(at, &["status", "--porcelain"]), "");
            // The derivation it built, not where it linked it.
            r.build()
                .split_whitespace()
                .find(|a| a.ends_with(".drv^*"))
                .expect("a derivation")
                .to_owned()
        };
        let tree = build(&d, true);
        assert!(
            tree.contains("-1-1.drv^*") && !tree.contains("-none-"),
            "{tree}"
        );
        for other in [&shallow, &worktree, &encoded] {
            assert_eq!(
                build(other, true),
                tree,
                "whole {whole}: {}",
                other.display()
            );
        }
        // Without it, the full and the shallow checkout of one commit, and
        // two commits of one tree, are other sources: the test can tell.
        let (full, cut, older) = (
            build(&d, false),
            build(&shallow, false),
            build(&worktree, false),
        );
        assert!(
            full != cut && full != older,
            "whole {whole}: {full} {cut} {older}"
        );
    }
    assert_eq!(g.s.git(&d, &["symbolic-ref", "HEAD"]), "refs/heads/main");
}

/// gate-reuse, without eval-apart (whose snapshot refuses it already): a
/// lock file that needs changing fails the gate, rather than pass on inputs
/// the tree doesn't lock, and the checkout's is left as it was.
#[test]
fn with_gate_reuse_the_lock_file_is_the_trees() {
    let g = Gate::new();
    let dep = g.flake("{ }");
    let d = g.flake_with(
        r#"{ checks.${system}.a = drv "a"; }"#,
        &format!(r#"inputs.dep.url = "path:{}";"#, dep.display()),
    );
    let r = g.run(
        &d,
        Run {
            whole: true,
            env: &[("TREE_SOURCE", "true")],
            ..Run::default()
        },
    );
    assert!(
        r.fails_with("lock file") && r.builds().is_empty(),
        "{}",
        r.out
    );
    assert!(!d.join("flake.lock").exists());
}

/// The plan is as pure as the evaluations: the runner's environment hides no
/// check.
#[test]
fn the_plan_is_pure() {
    let g = Gate::new();
    let d = g.flake(
        r#"{
      checks.${system} = if builtins.getEnv "GATE_ENV" == "" then { a = drv "a"; pure = throw "only pure"; } else { a = drv "a"; };
    }"#,
    );
    let r = g.run(
        &d,
        Run {
            env: &[("GATE_ENV", "set")],
            ..Run::default()
        },
    );
    assert!(r.fails_with("only pure"), "{}", r.out);
}

/// A checkout with changes not yet committed is evaluated as it is, purely,
/// with the source's own metadata.
#[test]
fn an_uncommitted_checkout_is_evaluated_as_it_is() {
    let g = Gate::new();
    let d = g.flake(r#"{ checks.${system}.a = drv "a"; }"#);
    let system = &g.system;
    write(
        &d.join("flake.nix"),
        &format!(
            r#"{{
  outputs = {{ self }}: {{
    checks.{system}.changed = {changed};
    checks.{system}.pure = if builtins.getEnv "GATE_ENV" == "" then throw "only pure" else null;
    # What nix flake check gives self for a checkout with changes.
    checks.{system}.meta = if self ? dirtyShortRev && self.lastModified > 0
      then derivation {{ name = "meta-${{builtins.replaceStrings [ "-" ] [ "" ] self.dirtyShortRev}}"; system = "{system}"; builder = "/bin/sh"; args = [ "-c" "echo > $out" ]; }}
      else throw "lost the source's metadata";
  }};
}}
"#,
            changed = g.drv("changed")
        ),
    );
    let r = g.run(
        &d,
        Run {
            env: &[("GATE_ENV", "set")],
            ..Run::default()
        },
    );
    assert!(r.fails_with("only pure"), "{}", r.out);
    let b = r.build();
    assert!(b.contains("-changed.drv^*"), "{b}");
    let meta = b.split_whitespace().any(|a| {
        a.strip_suffix("dirty.drv^*")
            .and_then(|a| a.rsplit_once("-meta-"))
            .is_some_and(|(_, h)| !h.is_empty() && h.bytes().all(|c| c.is_ascii_hexdigit()))
    });
    assert!(meta, "{b}");
}

/// Rustup consults the working directory's toolchain file. A caller must
/// not choose the compiler of the source-pinned runner that judges it.
#[test]
fn the_runner_compiles_outside_the_callers_checkout() {
    let g = Gate::new();
    let d = g.flake(r#"{ checks.${system}.a = drv "a"; }"#);
    // Keep the real compiler Gate::new selected, but reject invocation in
    // the caller tree, where rustup could read its rust-toolchain.toml.
    let compiler = read(&g.s.join("compiler/bin/rustc"));
    crate::harness::executable(
        &g.s.join("compiler/bin/rustc"),
        &format!(
            "[ ! -e flake.nix ] || {{ echo 'compiler ran in caller checkout' >&2; exit 1; }}\n{compiler}"
        ),
    );
    let r = g.apart(&d);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.build().contains("-a.drv^*"), "{}", r.out);
}

/// Only Git's output terminator may be removed: trimming the caller path
/// can silently select a different repository, for root and nested flakes.
#[test]
fn checkout_path_whitespace_never_selects_a_sibling() {
    for suffix in [" ", "\t", "\r"] {
        let g = Gate::new();
        let sibling = g.flake(r#"{ checks.${system}.sibling = drv "sibling"; }"#);
        let caller = g.flake(r#"{ checks.${system}.caller = drv "caller"; }"#);
        let parent = g.dir();
        let caller_path = parent.join(format!("project{suffix}"));
        fs::rename(&sibling, parent.join("project")).unwrap();
        fs::rename(&caller, &caller_path).unwrap();
        // A nested flake with whitespace in its directory tests Git's
        // prefix as well as its top-level checkout path.
        for root in [&parent.join("project"), &caller_path] {
            let nested = root.join(format!("nested{suffix}"));
            fs::create_dir(&nested).unwrap();
            burrow_test::copy_file(root.join("flake.nix"), nested.join("flake.nix"));
            g.s.git(root, &["add", "."]);
            g.s.git(root, &["commit", "-qm", "nested flake"]);
        }
        for dir in [
            caller_path.clone(),
            caller_path.join(format!("nested{suffix}")),
        ] {
            let r = g.apart(&dir);
            assert_eq!(r.code, 0, "{dir:?}: {}", r.out);
            assert!(r.build().contains("-caller.drv^*"), "{dir:?}: {}", r.out);
            assert!(!r.build().contains("-sibling.drv^*"), "{dir:?}: {}", r.out);
        }
    }
}

/// Self-hosted CI can expose rustup's proxy without a default toolchain.
/// Provision the compiler with Nix instead of depending on that user state.
#[test]
fn a_rustup_proxy_with_no_default_is_not_used() {
    let g = Gate::new();
    let d = g.flake(r#"{ checks.${system}.a = drv "a"; }"#);
    g.s.stub(
        "rustc",
        "echo 'rustup could not choose a version: no default is configured' >&2\nexit 1\n",
    );
    g.s.stub("cc", "echo no caller linker >&2; exit 1\n");
    let r = g.apart(&d);
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(r.build().contains("-a.drv^*"), "{}", r.out);
}

/// The bootstrap compiles the runner with rustc directly, so it must name the
/// workspace's edition: Cargo and Clippy check the source under that one, and
/// syntax only it accepts would otherwise break every gate before it starts.
#[test]
fn the_runner_compiles_under_the_workspaces_edition() {
    let cargo = read(&crate::harness::repo_file("crates/Cargo.toml"));
    let edition = cargo
        .lines()
        .find_map(|l| l.trim().strip_prefix("edition = "))
        .expect("crates/Cargo.toml names an edition")
        .trim_matches('"');
    let run = read(&crate::harness::repo_file(".github/flake-check/run.sh"));
    let named: Vec<_> = run
        .split_whitespace()
        .filter_map(|w| w.strip_prefix("--edition="))
        .collect();
    assert_eq!(named, [edition], "run.sh's rustc editions");
}
