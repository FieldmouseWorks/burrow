//! The checkout the gate reads: one snapshot of it, as nix flake check reads
//! it, its settings, submodules and uncommitted changes included, purely.

use std::fs;
use std::os::unix::fs::PermissionsExt;
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
                fs::copy(&filemode, d.join(".git/filemode.inc")).unwrap();
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
        fs::set_permissions(d.join("flake.nix"), fs::Permissions::from_mode(0o755)).unwrap();
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
