use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use syn::visit::Visit;

use super::{Guard, check};

#[test]
fn workspace_executables_use_the_child_installer() {
    let root = std::env::var_os("BURROW_TEST_SOURCE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::harness::repo_file("crates"));
    let findings = check(&root);
    assert!(
        findings.is_empty(),
        "{}",
        findings.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn setters_cannot_bypass_the_guard() {
    for code in [
        "fs::set_permissions(p, Permissions::from_mode(0o755)).unwrap();",
        "use std::fs::set_permissions as change; change(p, mode);",
        "p.set_mode(0o755);",
        "fs::copy(source, destination).unwrap();",
        "use std::fs::copy as duplicate; duplicate(source, destination);",
        "File::options().mode(0o755);",
        "Command::new(\"chmod\").args([\"+x\", \"stand-in\"]);",
        "Command::new(\"sh\").arg(\"chmod +x stand-in\");",
        "wrapper!(fs::set_permissions(p, mode));",
        "fs::write(&p, \"#!/bin/sh\\nexit 0\\n\").unwrap();",
        "File::create(&p).unwrap().write_all(b\"#!/bin/sh\\n\").unwrap();",
        "let gh = bin.join(\"gh\"); burrow_test::executable(&gh, GH); fs::write(&gh, OTHER).unwrap();",
        "executable(bin.join(\"gh\"), GH); File::create(dir.path().join(\"bin/gh\")).unwrap();",
    ] {
        let ast = syn::parse_file(&format!("fn fixture() {{ {code} }}")).unwrap();
        let mut guard = Guard::new("fixture.rs".into(), true);
        guard.visit_file(&ast);
        assert!(!guard.finish().is_empty(), "accepted: {code}");
    }
}

/// The usage tests' notice capture (#902): the rig installs `notify`, and the
/// test wrote its new body over it in place, keeping its mode.
#[test]
fn writing_over_a_stand_in_is_rejected() {
    let rig = r##"
        fn rig() {
            let notify = d.path().join("notify");
            burrow_test::executable(&notify, format!("#!/bin/sh\necho \"$1\" >>{dir}/told\n"));
            cfg["notify"] = json!(notify);
        }
    "##;
    let overwrite = r##"
        #[test]
        fn old_holds_are_normalized_for_status_reminders_and_extensions() {
            fs::write(
                cfg.notify.as_ref().unwrap(),
                format!("#!/bin/sh\nprintf '%s' \"$4\" >'{}'\n", capture.display()),
            )
            .unwrap();
        }
    "##;
    let found = |code: &str| {
        let mut guard = Guard::new("usage/tests.rs".into(), true);
        guard.visit_file(&syn::parse_file(code).unwrap());
        guard.finish()
    };
    let old = found(&format!("{rig}{overwrite}"));
    assert!(old.iter().any(|f| f.contains("script in place")), "{old:?}");
    // Its path alone gives it away, whatever it writes.
    let renamed = overwrite.replace("format!(\"#!", "format!(\"");
    let renamed = found(&format!("{rig}{renamed}"));
    assert!(
        renamed
            .iter()
            .any(|f| f.contains("over the stand-in `notify`")),
        "{renamed:?}"
    );
    let fixed = overwrite.replace("fs::write(", "burrow_test::executable(");
    let fixed = fixed.replace(".unwrap();\n        }", ";\n        }");
    assert_eq!(found(&format!("{rig}{fixed}")), BTreeSet::new());
    // Data beside a stand-in is still written plainly.
    let data = "#[test] fn data() { executable(bin.join(\"gh\"), GH); fs::write(bin.join(\"says\"), \"[]\").unwrap(); }";
    assert_eq!(found(data), BTreeSet::new());
}

fn fixture_findings(code: &str) -> BTreeSet<String> {
    let mut guard = Guard::new("fixture.rs".into(), true);
    guard.visit_file(&syn::parse_file(code).unwrap());
    guard.finish()
}

/// A write is no safer for sitting in a macro, coming through a `use`, or
/// reaching its stand-in through a typed, shadowed or reassigned local.
#[test]
fn writes_cannot_hide_from_the_guard() {
    for code in [
        // Macro arguments, nested macros and statement bodies.
        "assert!(fs::write(cfg.notify.as_ref().unwrap(), \"#!/bin/sh\\n\").is_ok());",
        "executable(cfg.notify.as_ref().unwrap(), N); assert!(fs::write(cfg.notify.as_ref().unwrap(), OTHER).is_ok());",
        "wrapper!(assert!(File::create(&p).unwrap().write_all(b\"#!/bin/sh\\n\").is_ok()));",
        "executable(bin.join(\"gh\"), GH); assert!(File::create(bin.join(\"gh\")).is_ok());",
        "assert_eq!(executable(bin.join(\"gh\"), GH), ()); fs::write(bin.join(\"gh\"), OTHER).unwrap();",
        "wrapper! { let p = bin.join(\"gh\"); executable(&p, GH); fs::write(&p, OTHER).unwrap(); }",
        // Direct, grouped, renamed, module and glob imports.
        "use std::fs::write; write(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs::{write as put}; put(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::{fs::{self as files}}; files::write(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs as files; use files::write as put; put(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs::*; write(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs::write; executable(bin.join(\"gh\"), GH); write(bin.join(\"gh\"), OTHER).unwrap();",
        "use std::fs::File as F; executable(bin.join(\"gh\"), GH); F::create(bin.join(\"gh\")).unwrap();",
        "use burrow_test::executable as script; script(bin.join(\"gh\"), GH); fs::write(bin.join(\"gh\"), OTHER).unwrap();",
        "use burrow_test::{self as bt}; use bt::executable as script; script(bin.join(\"gh\"), GH); fs::write(bin.join(\"gh\"), OTHER).unwrap();",
        // Typed bindings, outer bindings after a nested scope, reassignment.
        "let p: PathBuf = bin.join(\"gh\"); executable(&p, GH); fs::write(&p, OTHER).unwrap();",
        "let p = bin.join(\"gh\"); executable(&p, GH); { let p = dir.join(\"data\"); fs::write(&p, DATA).unwrap(); } fs::write(&p, OTHER).unwrap();",
        "let mut p = dir.join(\"data\"); executable(bin.join(\"gh\"), GH); p = bin.join(\"gh\"); fs::write(&p, OTHER).unwrap();",
        "let mut p = dir.join(\"data\"); executable(bin.join(\"gh\"), GH); { p = bin.join(\"gh\"); } fs::write(&p, OTHER).unwrap();",
        // Assignments inside assertion, vector and wrapper macros, nested
        // macros and statement bodies.
        "let mut p = bin.join(\"data\"); executable(bin.join(\"gh\"), GH); assert_eq!(p = bin.join(\"gh\"), ()); fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"data\"); executable(bin.join(\"gh\"), GH); let _v = vec![p = bin.join(\"gh\")]; fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"data\"); executable(bin.join(\"gh\"), GH); wrapper!(p = bin.join(\"gh\")); fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"data\"); executable(bin.join(\"gh\"), GH); wrapper!(assert_eq!(p = bin.join(\"gh\"), ())); fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"data\"); executable(bin.join(\"gh\"), GH); wrapper! { p = bin.join(\"gh\"); } fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"data\"); executable(bin.join(\"gh\"), GH); assert_eq!(p = bin.join(\"gh\"), (), \"{x}\", x = 1); fs::write(&p, OTHER).unwrap();",
        // Imports through a glob of an alias, declared out of order, reached
        // through `self::`, and inside a macro's statements.
        "use std::fs as files; use files::*; write(&p, \"#!/bin/sh\\n\").unwrap();",
        "use std::fs as files; use files::*; executable(bin.join(\"gh\"), GH); File::create(bin.join(\"gh\")).unwrap();",
        "use files::write as put; use std::fs as files; put(&p, \"#!/bin/sh\\n\").unwrap();",
        "use inner::write as put; use files::{self as inner}; use std::fs as files; put(&p, \"#!/bin/sh\\n\").unwrap();",
        "use bt::executable as script; use burrow_test as bt; script(bin.join(\"gh\"), GH); fs::write(bin.join(\"gh\"), OTHER).unwrap();",
        "use std::fs as files; self::files::write(&p, \"#!/bin/sh\\n\").unwrap();",
        "use self::files::write as put; use std::fs as files; put(&p, \"#!/bin/sh\\n\").unwrap();",
        "wrapper! { use std::fs::write as put; put(&p, \"#!/bin/sh\\n\").unwrap(); }",
        "wrapper! { use burrow_test::executable as script; script(bin.join(\"gh\"), GH); fs::write(bin.join(\"gh\"), OTHER).unwrap(); }",
        // An `if let` binding doesn't reach its `else`.
        "let p = bin.join(\"gh\"); executable(&p, GH); if let Some(p) = option {} else { fs::write(&p, OTHER).unwrap(); }",
        "let p = bin.join(\"gh\"); executable(&p, GH); if let Some(p) = option {} else if ready { fs::write(&p, OTHER).unwrap(); }",
        // What code that may not run assigns leaves what the local held.
        "let mut p = bin.join(\"gh\"); executable(&p, GH); if ready { p = bin.join(\"says\"); } fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); if ready { p = helper(); } else { p = bin.join(\"says\"); } fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); match ready { true => p = bin.join(\"says\"), false => {} } fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); for _ in helpers() { p = bin.join(\"says\"); } fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); while ready { p = bin.join(\"says\"); } fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); loop { if ready { break; } p = bin.join(\"says\"); } fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); 'out: { if ready { break 'out; } p = bin.join(\"says\"); } fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); let _later = || p = bin.join(\"says\"); fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); let _later = async { p = bin.join(\"says\"); }; fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); let _ = ready && { p = bin.join(\"says\"); true }; fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); let Some(_x) = option else { p = bin.join(\"says\"); return; }; fs::write(&p, OTHER).unwrap();",
        "let mut p = bin.join(\"gh\"); executable(&p, GH); debug_assert!({ p = bin.join(\"says\"); true }); fs::write(&p, OTHER).unwrap();",
    ] {
        let found = fixture_findings(&format!("fn fixture() {{ {code} }}"));
        assert!(!found.is_empty(), "accepted: {code}");
    }
    // A test module sees its parent's imports through `use super::*`.
    let module = "use std::fs::write as put;\nmod tests { use super::*; fn t() { put(&p, \"#!/bin/sh\\n\").unwrap(); } }";
    assert!(!fixture_findings(module).is_empty(), "accepted: {module}");
    let module =
        "mod tests { use std::fs::write as put; fn t() { put(&p, \"#!/bin/sh\\n\").unwrap(); } }";
    assert!(!fixture_findings(module).is_empty(), "accepted: {module}");
    // A module reaches its parent's aliases through `super::`.
    let module = "use std::fs as files;\nmod tests { fn t() { super::files::write(&p, \"#!/bin/sh\\n\").unwrap(); } }";
    assert!(!fixture_findings(module).is_empty(), "accepted: {module}");
    let module = "mod tests { use super::files::write as put; fn t() { put(&p, \"#!/bin/sh\\n\").unwrap(); } }\nuse std::fs as files;";
    assert!(!fixture_findings(module).is_empty(), "accepted: {module}");
}

/// Only a formatting macro's format string starts its named arguments:
/// in any other macro, `name = value` after a string assigns.
#[test]
fn macro_assignments_are_seen_after_strings() {
    let switch = "let mut p = bin.join(\"data\"); executable(bin.join(\"gh\"), GH);";
    for code in [
        "dbg!(\"switch\", p = bin.join(\"gh\"));",
        "wrapper!(\"switch\", p = bin.join(\"gh\"));",
        "wrapper!(dbg!(\"switch\", p = bin.join(\"gh\")));",
        "dbg!(wrapper!(\"switch\", p = bin.join(\"gh\")));",
        // A string in a formatting macro's operands isn't its format string.
        "assert_eq!(\"switch\", p = bin.join(\"gh\"));",
        "assert!(\"switch\".is_empty(), p = bin.join(\"gh\"));",
    ] {
        let code = format!("fn fixture() {{ {switch} {code} fs::write(&p, OTHER).unwrap(); }}");
        assert!(!fixture_findings(&code).is_empty(), "accepted: {code}");
    }
    let says = "let p = bin.join(\"says\"); executable(bin.join(\"gh\"), GH);";
    for code in [
        "println!(\"{p:?}\", p = bin.join(\"gh\"));",
        "assert_eq!(a, b, \"{p:?}\", p = bin.join(\"gh\"));",
        "assert!(ready, \"{p:?}\", p = bin.join(\"gh\"));",
        "write!(out, \"{p:?}\", p = bin.join(\"gh\")).unwrap();",
        "wrapper!(std::println!(\"{p:?}\", p = bin.join(\"gh\")));",
    ] {
        let code = format!("fn fixture() {{ {says} {code} fs::write(&p, \"[]\").unwrap(); }}");
        assert_eq!(fixture_findings(&code), BTreeSet::new(), "{code}");
    }
}

/// Each `use` means what it meant where it was declared: a child's import
/// of its parent's alias reaches what the alias stands for, and an inner
/// alias of the same name changes nothing an outer `use` brought in.
#[test]
fn imports_resolve_where_they_were_declared() {
    let script = "(&p, \"#!/bin/sh\\n\").unwrap();";
    let over = "(bin.join(\"gh\"), GH); fs::write(bin.join(\"gh\"), OTHER).unwrap();";
    for code in [
        // A parent's import, by the same name and renamed.
        format!(
            "use std::fs as files; mod tests {{ use super::files; fn t() {{ files::write{script} }} }}"
        ),
        format!(
            "use std::fs as files; mod tests {{ use super::files as f; fn t() {{ f::write{script} }} }}"
        ),
        format!(
            "use burrow_test as bt; mod tests {{ use super::bt; fn t() {{ bt::executable{over} }} }}"
        ),
        format!(
            "use burrow_test as bt; mod tests {{ use super::bt as b; fn t() {{ b::executable{over} }} }}"
        ),
        // Through `self::`, `super::super::` and `crate::`.
        format!(
            "use std::fs as files; mod a {{ mod tests {{ use super::super::files::write as put; fn t() {{ put{script} }} }} }}"
        ),
        format!(
            "use std::fs as files; mod tests {{ use crate::files::write as put; fn t() {{ put{script} }} }}"
        ),
        format!(
            "use std::fs as files; mod tests {{ use self::super::files; fn t() {{ files::write{script} }} }}"
        ),
        format!(
            "use burrow_test as bt; mod tests {{ use crate::bt::executable as script; fn t() {{ script{over} }} }}"
        ),
        format!(
            "use burrow_test as bt; mod a {{ mod tests {{ use super::super::bt; fn t() {{ bt::executable{over} }} }} }}"
        ),
        // An inner alias of the name an outer explicit or glob import used.
        format!(
            "fn fixture() {{ use std::fs as files; use files::write as put; {{ use std::io as files; put{script} }} }}"
        ),
        format!(
            "fn fixture() {{ use std::fs as files; use files::*; {{ use std::io as files; write{script} }} }}"
        ),
        format!(
            "use std::fs as files; use files::write as put; mod tests {{ use std::io as files; fn t() {{ put{script} }} }}"
        ),
        format!(
            "fn fixture() {{ use burrow_test as bt; use bt::executable as script; {{ use other as bt; script{over} }} }}"
        ),
        format!(
            "fn fixture() {{ use burrow_test as bt; use bt::*; {{ use other as bt; executable{over} }} }}"
        ),
        format!(
            "use burrow_test as bt; use bt::*; mod tests {{ use other as bt; fn t() {{ executable{over} }} }}"
        ),
    ] {
        assert!(!fixture_findings(&code).is_empty(), "accepted: {code}");
    }
    // A child's own alias hides its parent's.
    for code in [
        "use std::fs as files; mod tests { use std::io as files; fn t() { executable(bin.join(\"gh\"), GH); files::write(bin.join(\"gh\"), \"[]\"); } }",
        "use burrow_test::executable as script; mod tests { use other::install as script; fn t() { script(bin.join(\"gh\"), GH); fs::write(bin.join(\"gh\"), \"[]\").unwrap(); } }",
    ] {
        assert_eq!(fixture_findings(code), BTreeSet::new(), "{code}");
    }
}

/// A loop's body and a closure may run again after what they or later code
/// assign: a write sees every path its next run may be given.
#[test]
fn writes_see_what_later_runs_assign() {
    let setup = "let mut p = bin.join(\"data\"); let mut q = bin.join(\"data\"); executable(bin.join(\"gh\"), GH);";
    for code in [
        "for _ in 0..2 { fs::write(&p, OTHER).unwrap(); p = bin.join(\"gh\"); }",
        "while ready { fs::write(&p, OTHER).unwrap(); p = bin.join(\"gh\"); }",
        "loop { fs::write(&p, OTHER).unwrap(); p = bin.join(\"gh\"); }",
        // A write in the condition, and a `while let`.
        "while fs::write(&p, OTHER).is_ok() { p = bin.join(\"gh\"); }",
        "while let Ok(()) = fs::write(&p, OTHER) { p = bin.join(\"gh\"); }",
        // Reached through `continue`, and through nested loops.
        "for x in xs { fs::write(&p, OTHER).unwrap(); if x { p = bin.join(\"gh\"); continue; } }",
        "for _ in xs { for _ in ys { fs::write(&p, OTHER).unwrap(); } p = bin.join(\"gh\"); }",
        // Two runs before the stand-in's path reaches the write.
        "loop { fs::write(&p, OTHER).unwrap(); p = q.clone(); q = bin.join(\"gh\"); }",
        // A closure called again after its captured path changes.
        "let w = || fs::write(&p, OTHER); w().unwrap(); p = bin.join(\"gh\"); w().unwrap();",
        "let mut w = || { fs::write(&p, OTHER).unwrap(); p = bin.join(\"gh\"); }; w(); w();",
        "let w = || fs::write(&q, OTHER); q = p.clone(); p = bin.join(\"gh\"); q = p.clone(); w().unwrap();",
        "let fut = async { fs::write(&p, OTHER).unwrap() }; p = bin.join(\"gh\"); fut.await;",
        "let install = || executable(&p, GH); p = bin.join(\"gh\"); install(); fs::write(bin.join(\"gh\"), OTHER).unwrap();",
    ] {
        let code = format!("fn fixture() {{ {setup} {code} }}");
        assert!(!fixture_findings(&code).is_empty(), "accepted: {code}");
    }
    for code in [
        "let p = bin.join(\"says\"); executable(bin.join(\"gh\"), GH); for _ in xs { fs::write(&p, \"[]\").unwrap(); }",
        "for _ in xs { let mut q = bin.join(\"gh\"); executable(&q, GH); q = bin.join(\"says\"); fs::write(&q, \"[]\").unwrap(); }",
        "let p = bin.join(\"says\"); executable(bin.join(\"gh\"), GH); let w = || fs::write(&p, \"[]\"); w().unwrap(); w().unwrap();",
        "let p = bin.join(\"says\"); executable(bin.join(\"gh\"), GH); loop { let w = |p: &Path| fs::write(p, \"[]\"); w(&p).unwrap(); }",
    ] {
        let code = format!("fn fixture() {{ {code} }}");
        assert_eq!(fixture_findings(&code), BTreeSet::new(), "{code}");
    }
}

/// What names nothing, or names something else by the time it's written,
/// is plain data beside a stand-in.
#[test]
fn data_beside_a_stand_in_is_still_written() {
    for code in [
        "fn install(notify: &Path) { executable(notify, N); } fn data(notify: &Path) { fs::write(notify, \"[]\").unwrap(); }",
        "fn fixture() { let mut p = bin.join(\"gh\"); executable(&p, GH); p = bin.join(\"says\"); fs::write(&p, \"[]\").unwrap(); }",
        "fn fixture() { let mut p = bin.join(\"gh\"); executable(&p, GH); p = helper(); fs::write(&p, \"[]\").unwrap(); }",
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); { let p = helper(); fs::write(&p, \"[]\").unwrap(); } }",
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); let (p, _) = (helper(), 0); fs::write(&p, \"[]\").unwrap(); }",
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); let put = |p: &Path| fs::write(p, \"[]\"); }",
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); for p in helpers() { fs::write(&p, \"[]\").unwrap(); } }",
        "fn fixture() { let p = bin.join(\"says\"); executable(bin.join(\"gh\"), GH); println!(\"{p:?}\", p = bin.join(\"gh\")); fs::write(&p, \"[]\").unwrap(); }",
        "use std::fs::write; fn fixture() { executable(bin.join(\"gh\"), GH); write(bin.join(\"says\"), \"[]\").unwrap(); }",
        // A local bound in a branch is reassigned for certain within it.
        "fn fixture() { if ready { let mut q = bin.join(\"gh\"); executable(&q, GH); q = bin.join(\"says\"); fs::write(&q, \"[]\").unwrap(); } }",
        // An `if let` binding hides the outer local in its own body.
        "fn fixture() { let p = bin.join(\"gh\"); executable(&p, GH); if let Some(p) = option { fs::write(&p, \"[]\").unwrap(); } }",
        // A branch that only reads the stand-in leaves the data local alone.
        "fn fixture() { let p = bin.join(\"says\"); executable(bin.join(\"gh\"), GH); if ready { let _q = bin.join(\"gh\"); } fs::write(&p, \"[]\").unwrap(); }",
        // A glob from anywhere but `fs` brings no writer in.
        "use std::io::*; fn fixture() { executable(bin.join(\"gh\"), GH); write(bin.join(\"gh\"), \"[]\"); }",
    ] {
        assert_eq!(fixture_findings(code), BTreeSet::new(), "{code}");
    }
}

#[test]
fn test_helpers_are_followed_regardless_of_their_names() {
    let dir = tempfile::tempdir().unwrap();
    let member = dir.path().join("example");
    fs::create_dir_all(member.join("src/fixtures")).unwrap();
    fs::write(member.join("Cargo.toml"), "").unwrap();
    fs::write(
        member.join("src/lib.rs"),
        "fn production() { fs::set_permissions(p, mode); }\n#[cfg(test)] mod fixtures;",
    )
    .unwrap();
    fs::write(member.join("src/fixtures.rs"), "mod arbitrary;").unwrap();
    fs::write(
        member.join("src/fixtures/arbitrary.rs"),
        "fn helper() { fs::set_permissions(p, Permissions::from_mode(0o755)); }",
    )
    .unwrap();
    let found = check(dir.path());
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(
        found.iter().all(|f| f.contains("arbitrary.rs")),
        "{found:?}"
    );
}
