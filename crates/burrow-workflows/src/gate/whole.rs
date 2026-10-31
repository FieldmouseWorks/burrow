//! The gate without eval-apart: the checks built first, by derivation, kept
//! until nix flake check has found them built.

use super::{EVERY, Gate, Run};
use crate::harness::write;

/// Without eval-apart, the checks are built by derivation, kept until nix
/// flake check has found them built; with a check it doesn't build, nix
/// flake check builds none.
#[test]
fn whole_the_checks_are_built_then_found_built() {
    let g = Gate::new();
    let every = g.flake(EVERY);
    let r = g.run(
        &every,
        Run {
            whole: true,
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(
        r.calls[..2],
        ["nix-instantiate plan", "nix-instantiate checks"],
        "{:?}",
        r.calls
    );
    assert_eq!(
        r.calls[3..],
        ["flake check -L --keep-going"],
        "{:?}",
        r.calls
    );
    let b = r.build();
    assert!(
        b.starts_with("build -L --out-link ")
            && b.split_whitespace()
                .nth(3)
                .is_some_and(|l| l.contains("/roots.")),
        "{b}"
    );
    assert!(
        b.contains("-a.drv^*") && b.contains("-vm.drv^*") && !b.contains("-p.drv"),
        "{b}"
    );
    let r = g.run(
        &every,
        Run {
            whole: true,
            skip: "vm",
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(
        r.calls[..2],
        ["nix-instantiate plan", "nix-instantiate checks"],
        "{:?}",
        r.calls
    );
    assert_eq!(r.calls[3..], ["flake check --no-build -L"], "{:?}", r.calls);
    let b = r.build();
    assert!(b.ends_with("-a.drv^*") && !b.contains("-vm.drv"), "{b}");
    assert!(r.out.contains("not building vm"), "{}", r.out);
}

/// Without eval-apart, a check that fails to build is built once, and one
/// that doesn't evaluate leaves nix flake check the rest.
#[test]
fn whole_a_failing_check() {
    let g = Gate::new();
    let r = g.run(
        &g.flake(EVERY),
        Run {
            whole: true,
            build_fail: "-a.drv",
            ..Run::default()
        },
    );
    assert_ne!(r.code, 0, "{}", r.out);
    assert!(
        r.builds().len() == 1
            && r.calls
                .last()
                .is_some_and(|c| c == "flake check --no-build -L --keep-going"),
        "{:?}",
        r.calls
    );
    let broken =
        g.flake(r#"{ checks.${system} = { a = drv "a"; broken = throw "broken check"; }; }"#);
    for skip in ["", "x"] {
        let r = g.run(
            &broken,
            Run {
                whole: true,
                skip,
                ..Run::default()
            },
        );
        assert!(r.fails_with("broken check"), "{}", r.out);
        assert!(
            r.builds().is_empty() && r.calls[2].starts_with("flake check"),
            "{:?}",
            r.calls
        );
    }
}

/// Without eval-apart, no checks, no build.
#[test]
fn whole_no_checks_no_build() {
    let g = Gate::new();
    let r = g.run(
        &g.flake(r#"{ packages.${system}.p = drv "p"; }"#),
        Run {
            whole: true,
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(
        r.calls,
        [
            "nix-instantiate plan",
            "nix-instantiate checks",
            "flake check -L --keep-going"
        ]
    );
}

/// Without eval-apart, a flake with a nixConfig is nix flake check's alone,
/// which applies the config a runner accepts.
#[test]
fn whole_a_nix_config_is_nix_flake_checks() {
    let g = Gate::new();
    let d = g.flake_with(
        r#"{ checks.${system} = { a = drv "a"; vm = drv "vm"; }; }"#,
        r#"nixConfig.bash-prompt = "x";"#,
    );
    let r = g.run(
        &d,
        Run {
            whole: true,
            ..Run::default()
        },
    );
    assert!(
        r.code == 0 && r.calls == ["nix-instantiate plan", "flake check -L --keep-going"],
        "{} {:?}",
        r.out,
        r.calls
    );
    let r = g.run(
        &d,
        Run {
            whole: true,
            skip: "vm",
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(
        r.calls,
        [
            "nix-instantiate plan".to_owned(),
            "flake check --no-build -L".to_owned(),
            format!("build -L --no-link --keep-going .#checks.{}.a", g.system)
        ]
    );
    let r = g.run(
        &d,
        Run {
            whole: true,
            skip: "vm",
            build_fail: ".a",
            ..Run::default()
        },
    );
    assert_ne!(r.code, 0, "{} {:?}", r.out, r.calls);
}

/// Without eval-apart, the flake is only read purely: its flake.nix never
/// sees the runner's environment.
#[test]
fn whole_the_flake_is_read_purely() {
    let g = Gate::new();
    let d = g.dir();
    write(
        &d.join("flake.nix"),
        "builtins.trace \"secret=${builtins.getEnv \"GATE_SECRET\"}\" { nixConfig = { }; outputs = { self }: { }; }\n",
    );
    g.commit_all(&d);
    for skip in ["", "x"] {
        let r = g.run(
            &d,
            Run {
                whole: true,
                skip,
                env: &[("GATE_SECRET", "hunter2")],
                ..Run::default()
            },
        );
        assert!(r.code != 0 && !r.out.contains("hunter2"), "{}", r.out);
    }
}
