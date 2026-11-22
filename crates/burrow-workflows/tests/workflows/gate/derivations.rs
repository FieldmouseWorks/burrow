//! The derivations the gate takes: anything but a derivation, or one nix
//! flake check wouldn't read, fails it, wherever it is and built or not.

use super::{Gate, Run};

/// Anything but a derivation fails it, built or not.
#[test]
fn anything_but_a_derivation_fails_it() {
    let g = Gate::new();
    let system = &g.system;
    let fake = format!("flake attribute 'checks.{system}.fake' is not a derivation");
    for (outputs, skip, error) in [
        (
            r#"checks.${system} = { ok = drv "ok"; fake = { drvPath = ""; }; };"#,
            "",
            fake.clone(),
        ),
        (
            r#"checks.${system} = { ok = drv "ok"; fake = { drvPath = ""; }; };"#,
            "fake",
            fake,
        ),
        (
            r#"checks.${system} = { ok = drv "ok"; fake = { type = "derivation"; name = "x"; }; };"#,
            "",
            "drvPath".to_owned(),
        ),
        (
            r#"checks.${system} = { ok = drv "ok"; fake = { type = "derivation"; name = "x"; drvPath = "/nix/store/00000000000000000000000000000000-x.drv"; }; };"#,
            "",
            "is not valid".to_owned(),
        ),
        (
            r#"checks.${system}.ok = drv "ok"; packages.${system}.fake = "/nix/store/x";"#,
            "",
            format!("flake attribute 'packages.{system}.fake' is not a derivation"),
        ),
        (
            r#"checks.${system}.ok = drv "ok"; devShells.${system}.fake = { };"#,
            "",
            format!("flake attribute 'devShells.{system}.fake' is not a derivation"),
        ),
        (
            r#"checks.${system}.ok = drv "ok"; formatter.${system} = 1;"#,
            "",
            format!("flake attribute 'formatter.{system}' is not a derivation"),
        ),
        (
            r#"checks.${system}.ok = drv "ok"; nixosConfigurations.box.config.system.build.toplevel = { };"#,
            "",
            "'config.system.build.toplevel' of nixosConfigurations.box is not a derivation"
                .to_owned(),
        ),
    ] {
        let r = g.run(
            &g.flake(&format!("{{ {outputs} }}")),
            Run {
                skip,
                ..Run::default()
            },
        );
        assert!(r.fails_with(&error), "{outputs}: {}", r.out);
        // The rest are still built.
        assert!(
            r.builds().iter().any(|b| b.contains("-ok.drv^*")),
            "{outputs}: {:?}",
            r.calls
        );
    }
}

/// Where a derivation under test goes, and the check skipped.
#[derive(Clone, Copy)]
enum At {
    Check,
    SkippedCheck,
    Package,
    DevShell,
    Formatter,
}

impl At {
    fn outputs(self, value: &str) -> (String, &'static str) {
        let ok = r#"checks.${system}.ok = drv "ok";"#;
        match self {
            At::Check => (
                format!(r#"checks.${{system}} = {{ ok = drv "ok"; fake = {value}; }};"#),
                "",
            ),
            At::SkippedCheck => (
                format!(r#"checks.${{system}} = {{ ok = drv "ok"; fake = {value}; }};"#),
                "fake",
            ),
            At::Package => (format!("{ok} packages.${{system}}.fake = {value};"), ""),
            At::DevShell => (format!("{ok} devShells.${{system}}.fake = {value};"), ""),
            At::Formatter => (format!("{ok} formatter.${{system}} = {value};"), ""),
        }
    }

    /// The same place, for a flake with nothing else in it.
    fn alone(self, value: &str) -> (String, &'static str) {
        match self {
            At::Check => (format!("checks.${{system}}.fake = {value};"), ""),
            At::SkippedCheck => (format!("checks.${{system}}.fake = {value};"), "fake"),
            At::Package => (format!("packages.${{system}}.fake = {value};"), ""),
            At::DevShell => (format!("devShells.${{system}}.fake = {value};"), ""),
            At::Formatter => (format!("formatter.${{system}} = {value};"), ""),
        }
    }
}

/// A derivation nix flake check wouldn't read fails it, as a check built or
/// skipped, a package, a dev shell or a formatter.
fn invalid_derivations_fail(at: At) {
    let g = Gate::new();
    for (value, error) in [
        (r#"drv "x" // { name = throw "bad name"; }"#, "bad name"),
        (
            r#"removeAttrs (drv "x") [ "name" ]"#,
            "derivation name missing",
        ),
        (
            r#"drv "x" // { name = 1; }"#,
            "is not a string without context",
        ),
        (
            r#"drv "x" // { name = "${drv "y"}"; }"#,
            "is not a string without context",
        ),
        (
            r#"drv "x" // { drvPath = throw "bad drvPath"; }"#,
            "bad drvPath",
        ),
        (
            r#"drv "x" // { drvPath = ""; }"#,
            "is not a store derivation's path",
        ),
        (
            r#"drv "x" // { drvPath = (drv "x").drvPath + "\n"; }"#,
            "is not a store derivation's path",
        ),
        (
            r#"drv "x" // { drvPath = " " + (drv "x").drvPath; }"#,
            "is not a store derivation's path",
        ),
        (
            r#"drv "x" // { drvPath = (drv "x").outPath; }"#,
            "is not a store derivation's path",
        ),
        (
            r#"drv "x" // { drvPath = "/elsewhere/00000000000000000000000000000000-x.drv"; }"#,
            "is not a store derivation's path",
        ),
        (r#"drv "x" // { drvPath = 1; }"#, "cannot coerce"),
        (
            r#"drv "x" // { drvPath = { outPath = "/nix/store/x.drv"; }; }"#,
            "is not a store derivation's path",
        ),
        (
            r#"drv "x" // { drvPath = ./flake.nix; }"#,
            "is not a store derivation's path",
        ),
        (
            r#"drv "x" // { drvPath = { __toString = _: "/nix/store/x.drv"; outPath = (drv "x").drvPath; }; }"#,
            "is not a store derivation's path",
        ),
        (
            r#"drv "x" // { drvPath = { __toString = _: ./flake.nix; }; }"#,
            "is not a store derivation's path",
        ),
        (
            r#"drv "x" // { drvPath = "/nix/store/00000000000000000000000000000000-${builtins.concatStringsSep "" (builtins.genList (_: "x") 208)}.drv"; }"#,
            "is not a store derivation's path",
        ),
    ] {
        let (outputs, skip) = at.outputs(value);
        // A NixOS system alongside, whose own result is no derivation path.
        let d = g.flake(&format!(
            "{{\n  nixosConfigurations.box.config.system.build.toplevel = drv \"box\";\n  {outputs}\n}}"
        ));
        let r = g.run(
            &d,
            Run {
                skip,
                ..Run::default()
            },
        );
        assert!(r.fails_with(error), "{value} {outputs}: {}", r.out);
        assert!(
            r.builds().iter().any(|b| b.contains("-ok.drv^*")),
            "{value} {outputs}: {} {:?}",
            r.out,
            r.calls
        );
    }
}

#[test]
fn an_invalid_check_fails_it() {
    invalid_derivations_fail(At::Check);
}

#[test]
fn an_invalid_skipped_check_fails_it() {
    invalid_derivations_fail(At::SkippedCheck);
}

#[test]
fn an_invalid_package_fails_it() {
    invalid_derivations_fail(At::Package);
}

#[test]
fn an_invalid_dev_shell_fails_it() {
    invalid_derivations_fail(At::DevShell);
}

#[test]
fn an_invalid_formatter_fails_it() {
    invalid_derivations_fail(At::Formatter);
}

/// A derivation nix flake check reads passes, its name dotted or its
/// drvPath a path or an attribute set, wherever it is.
fn valid_derivations_pass(at: At) {
    let g = Gate::new();
    for value in [
        r#"drv ".check""#,
        r#"drv "x" // { drvPath = { outPath = (drv "x").drvPath; }; }"#,
        r#"drv "x" // { drvPath = { outPath = /. + builtins.unsafeDiscardStringContext (drv "x").drvPath; }; }"#,
        r#"drv "x" // { drvPath = /. + builtins.unsafeDiscardStringContext (drv "x").drvPath; }"#,
        r#"drv "x" // { drvPath = { __toString = _: /. + builtins.unsafeDiscardStringContext (drv "x").drvPath; }; }"#,
        r#"drv "x" // { drvPath = { __toString = _: { outPath = { __toString = _: (drv "x").drvPath; }; }; }; }"#,
        r#"drv "x" // { drvPath = { __toString = _: (drv "x").drvPath; outPath = "/nix/store/x.drv"; }; }"#,
    ] {
        let (outputs, skip) = at.alone(value);
        let r = g.run(
            &g.flake(&format!("{{ {outputs} }}")),
            Run {
                skip,
                ..Run::default()
            },
        );
        assert_eq!(r.code, 0, "{value} {outputs}: {}", r.out);
    }
}

#[test]
fn a_valid_check_passes() {
    valid_derivations_pass(At::Check);
}

#[test]
fn a_valid_skipped_check_passes() {
    valid_derivations_pass(At::SkippedCheck);
}

#[test]
fn a_valid_package_passes() {
    valid_derivations_pass(At::Package);
}

#[test]
fn a_valid_dev_shell_passes() {
    valid_derivations_pass(At::DevShell);
}

#[test]
fn a_valid_formatter_passes() {
    valid_derivations_pass(At::Formatter);
}
