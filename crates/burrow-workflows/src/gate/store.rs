//! The gate with a build store named (BUILDS, from the caller's Actions
//! variables; crates/burrow-flake-check/src/builds.rs): before it builds,
//! it says what the store has of the derivations it builds cold, and
//! substitutes nothing from it; every build it starts refuses a flake's own
//! config and is sandboxed, though the runner's config would take a
//! flake's. With no store named, it builds as it did.

use super::*;

/// A cache with one output a check makes (a fixed output, which needs no
/// signature), in a directory standing in for the build store, and a flake
/// whose checks are that one and one no store has.
fn store_and_flake(g: &Gate) -> (PathBuf, PathBuf) {
    let file = g.s.join("fetched");
    write(&file, "built by an earlier run\n");
    let nix = |args: &[&str]| {
        let mut c = g.s.command(which("nix"));
        c.args(args)
            .env("NIX_CONFIG", &g.nix_config)
            .env("XDG_CACHE_HOME", g.s.join("cache"));
        let o = output(c);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8(o.stdout).unwrap().trim().to_owned()
    };
    let hash = nix(&[
        "hash",
        "file",
        "--type",
        "sha256",
        "--base32",
        file.to_str().unwrap(),
    ]);
    let path = nix(&["store", "add-file", file.to_str().unwrap()]);
    let cache = g.s.join("builds");
    nix(&[
        "copy",
        "--to",
        &format!("file://{}", cache.display()),
        &path,
    ]);
    nix(&["store", "delete", &path]);
    let fetched = format!(
        r#"derivation {{ name = "fetched"; inherit system; builder = "/bin/sh"; outputHashMode = "flat"; outputHashAlgo = "sha256"; outputHash = "{hash}"; }}"#
    );
    let flake = g.flake(&format!(
        r#"{{ checks.${{system}} = {{ fetched = {fetched}; cold = drv "cold"; }}; }}"#
    ));
    (cache, flake)
}

/// Each build's config, as the stand-in logged it.
fn configs(g: &Gate) -> Vec<String> {
    lines(&g.s.join("nix.log.config"))
}

#[test]
fn a_leg_says_what_the_store_has_and_takes_nothing_from_it() {
    let g = Gate::new();
    let (cache, flake) = store_and_flake(&g);
    let url = format!("file://{}", cache.display());
    // A runner whose own config would take a flake's.
    let runner = format!("{}\naccept-flake-config = true", g.nix_config);
    let ran = g.run(
        &flake,
        Run {
            env: &[
                ("NIX_CONFIG", &runner),
                ("BUILDS", &url),
                ("BUILDS_KEY", ""),
            ],
            ..Run::default()
        },
    );
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert!(
        ran.out.contains(
            "build store (shadow, nothing substituted): it has 1 of the 2 derivations this leg builds cold"
        ) && ran.out.contains("builds-shadow {"),
        "{}",
        ran.out
    );
    let shadow = ran
        .out
        .lines()
        .find_map(|l| l.strip_prefix("builds-shadow "))
        .unwrap();
    assert!(
        shadow.contains(r#""served_names":["fetched"]"#)
            && shadow.contains(r#""unserved_names":["cold"]"#),
        "{shadow}"
    );
    let built = configs(&g);
    assert!(!built.is_empty(), "{}", ran.out);
    for c in &built {
        assert!(
            !c.contains(cache.to_str().unwrap()),
            "a build substitutes from the store: {c}"
        );
        assert!(
            c.contains(r#""accept-flake-config":false"#) && c.contains(r#""sandbox":true"#),
            "a build takes a flake's config, or isn't sandboxed: {c}"
        );
    }

    // With no store named, nothing of it: the runner's config stands.
    let ran = g.run(
        &flake,
        Run {
            env: &[("NIX_CONFIG", &runner), ("BUILDS", ""), ("BUILDS_KEY", "")],
            ..Run::default()
        },
    );
    assert_eq!(ran.code, 0, "{}", ran.out);
    assert!(!ran.out.contains("build store"), "{}", ran.out);
    for c in configs(&g) {
        assert!(c.contains(r#""accept-flake-config":true"#), "{c}");
    }
}
