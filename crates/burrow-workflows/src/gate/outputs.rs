//! The outputs the gate checks, as nix flake check checks them, and what it
//! evaluates and builds of them.

use std::fs;

use super::{EVERY, Gate, Run};

/// How many evaluations the gate said it runs side by side.
fn side(out: &str) -> usize {
    out.lines()
        .find_map(|l| {
            l.strip_prefix("6 outputs to evaluate, ")?
                .strip_suffix(" side by side")
        })
        .unwrap_or_else(|| panic!("no count: {out}"))
        .parse()
        .unwrap()
}

/// Each derivation on its own, kept until built, then only the checks built.
#[test]
fn each_derivation_apart() {
    let g = Gate::new();
    let r = g.run(
        &g.flake(EVERY),
        Run {
            skip: "vm",
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    // One plan, then one evaluation per derivation, each said in the plan's
    // order, however they ran.
    assert_eq!(r.calls[0], "eval plan", "{:?}", r.calls);
    let mut entries = r.entries();
    entries.sort();
    let want: Vec<String> = (0..6).map(|i| format!("eval entry {i}")).collect();
    assert_eq!(entries, want, "{:?}", r.calls);
    let system = &g.system;
    let said: Vec<&str> = r
        .out
        .lines()
        .filter_map(|l| l.strip_prefix("evaluating "))
        .collect();
    assert_eq!(
        said,
        [
            format!("checks.{system}.a"),
            format!("checks.{system}.vm"),
            format!("packages.{system}.p"),
            format!("devShells.{system}.default"),
            format!("formatter.{system}"),
            "nixosConfigurations.box".to_owned(),
        ],
        "{}",
        r.out
    );
    // What it builds is the derivation the evaluation gave, outputs and all.
    let b = r.build();
    assert!(
        b.starts_with("build -L --no-link --keep-going /nix/store/") && b.ends_with("-a.drv^*"),
        "{b}"
    );
    assert!(!b.contains("-vm.drv") && !b.contains("-p.drv"), "{b}");
    assert!(
        r.out.contains(&format!("not building checks.{system}.vm")),
        "{}",
        r.out
    );
}

/// A failing check fails it.
/// The outputs are evaluated side by side, as many at once as the step says
/// it takes (a core each while each has 2G of the memory free to it), and no
/// more.
#[test]
fn outputs_are_evaluated_side_by_side() {
    let g = Gate::new();
    let r = g.run(
        &g.flake(EVERY),
        Run {
            skip: "vm",
            env: &[("EVAL_OVERLAP", "1")],
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    // Each evaluation held a second, so those started together overlapped.
    let most = super::lines(&g.s.join("nix.log.overlap"))
        .iter()
        .map(|n| n.trim().parse::<usize>().unwrap())
        .max();
    assert_eq!(most, Some(side(&r.out).min(6)), "{}", r.out);
}

/// In a cgroup with a memory limit, only as many side by side as the limit
/// leaves room for, however much the machine has free: here 2G (3G, less
/// the 2G the cgroup holds, 1G of it inactive page cache), so one at a time.
#[test]
fn side_by_side_stays_within_a_cgroups_memory_limit() {
    under_a_limit(Cgroups::V2);
}

/// The same under cgroup v1's memory controller, whose "no limit" is a
/// number.
#[test]
fn side_by_side_stays_within_a_v1_memory_limit() {
    under_a_limit(Cgroups::V1);
}

enum Cgroups {
    V1,
    V2,
}

fn under_a_limit(v: Cgroups) {
    let g = Gate::new();
    // A stand-in for the cgroup filesystem, with the gate's own cgroup in it
    // and the limit at its top, so it's found only by walking every level up.
    let own = fs::read_to_string("/proc/self/cgroup").unwrap();
    let own = own
        .lines()
        .find_map(|l| {
            let [_, controllers, path] = l.splitn(3, ':').collect::<Vec<_>>()[..] else {
                return None;
            };
            let mine = match v {
                Cgroups::V2 => controllers.is_empty(),
                Cgroups::V1 => controllers.split(',').any(|c| c == "memory"),
            };
            mine.then_some(path)
        })
        .unwrap_or("/");
    let fs_top = g.s.join("cgroup");
    let (top, max, held, cache, none) = match v {
        Cgroups::V2 => (
            fs_top.clone(),
            "memory.max",
            "memory.current",
            "inactive_file",
            "max".to_owned(),
        ),
        Cgroups::V1 => (
            fs_top.join("memory"),
            "memory.limit_in_bytes",
            "memory.usage_in_bytes",
            "total_inactive_file",
            "9223372036854771712".to_owned(),
        ),
    };
    let mut at = top.clone();
    for part in own.split('/').filter(|p| !p.is_empty()) {
        at.push(part);
        fs::create_dir_all(&at).unwrap();
        fs::write(at.join(max), format!("{none}\n")).unwrap();
    }
    fs::create_dir_all(&top).unwrap();
    fs::write(top.join(max), format!("{}\n", 3u64 << 30)).unwrap();
    fs::write(top.join(held), format!("{}\n", 2u64 << 30)).unwrap();
    fs::write(
        top.join("memory.stat"),
        format!("anon 1073741824\nactive_file 0\n{cache} {}\n", 1u64 << 30),
    )
    .unwrap();
    let r = g.run(
        &g.flake(EVERY),
        Run {
            skip: "vm",
            env: &[("CGROUP_FS", fs_top.to_str().unwrap())],
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    assert_eq!(side(&r.out), 1, "{}", r.out);
}

#[test]
fn a_failing_check_fails_it() {
    let g = Gate::new();
    let r = g.run(
        &g.flake(EVERY),
        Run {
            build_fail: "-a.drv",
            ..Run::default()
        },
    );
    assert!(r.code != 0 && !r.builds().is_empty(), "{}", r.out);
}

/// A check that doesn't evaluate fails it, once the rest are evaluated and
/// built.
#[test]
fn a_check_that_doesnt_evaluate_fails_it() {
    let g = Gate::new();
    let r = g.apart(&g.flake(
        r#"{
      checks.${system} = { a = drv "a"; broken = throw "broken check"; };
      packages.${system}.p = drv "p";
    }"#,
    ));
    assert!(r.fails_with("broken check"), "{}", r.out);
    assert_eq!(r.entries().len(), 3, "{:?}", r.calls);
    assert!(r.build().contains("-a.drv^*"));
}

/// An output nix flake check checks some other way is refused, before
/// anything is evaluated.
#[test]
fn outputs_it_cant_check_are_refused() {
    let g = Gate::new();
    let r = g.apart(&g.flake(
        r#"{
      checks.${system}.a = drv "a";
      overlays.o = final: prev: { };
      apps.${system}.x = { type = "app"; program = "/bin/sh"; };
      lib.f = x: x;
    }"#,
    ));
    assert!(
        r.fails_with(r#"this flake has ["apps","overlays"]"#),
        "{}",
        r.out
    );
    assert!(
        r.entries().is_empty() && r.builds().is_empty(),
        "{:?}",
        r.calls
    );
}

/// Modules, templates, lib and unknown outputs are checked as nix flake
/// check checks them.
#[test]
fn other_outputs_are_checked_as_nix_flake_check_does() {
    let g = Gate::new();
    let r = g.apart(&g.flake(
        r#"{
      checks.${system}.a = drv "a";
      nixosModules = { m = { }; f = { config, ... }: throw "not applied"; };
      templates.t = { path = ./.; description = "a template"; welcomeText = "hi"; };
      lib.deep = throw "not forced";
      foo = { };
      "" = { };
    }"#,
    ));
    assert_eq!(r.code, 0, "{}", r.out);
    assert!(
        r.out.contains("warning: unknown flake output 'foo'")
            && r.out.contains("warning: unknown flake output ''"),
        "{}",
        r.out
    );
    assert!(
        !r.out.contains("warning: unknown flake output 'lib'"),
        "{}",
        r.out
    );
    assert!(
        r.entries().len() == 1 && !r.builds().is_empty(),
        "{:?}",
        r.calls
    );
    for (outputs, error) in [
        (r#"nixosModules.bad = throw "bad module";"#, "bad module"),
        (
            "nixosModules = null;",
            "flake output 'nixosModules' is not an attribute set",
        ),
        (r#"lib = throw "bad lib";"#, "bad lib"),
        (r#"foo = throw "bad foo";"#, "bad foo"),
        (
            "templates = null;",
            "flake output 'templates' is not an attribute set",
        ),
        (
            "templates.t = null;",
            "flake output 'templates.t' is not an attribute set",
        ),
        (
            r#"templates.t.description = "d";"#,
            "template 'templates.t' lacks attribute 'path'",
        ),
        (
            r#"templates.t = { path = ./nowhere; description = "d"; };"#,
            "refers to a non-existent path",
        ),
        (
            "templates.t.path = ./.;",
            "template 'templates.t' lacks attribute 'description'",
        ),
        (
            "templates.t = { path = ./.; description = 1; };",
            "is not a string without context",
        ),
        (
            r#"templates.t = { path = ./.; description = "d"; extra = 1; };"#,
            "has unsupported attribute 'extra'",
        ),
    ] {
        let r = g.apart(&g.flake(&format!(
            r#"{{ checks.${{system}}.a = drv "a"; {outputs} }}"#
        )));
        assert!(r.fails_with(error), "{outputs}: {}", r.out);
        assert!(r.entries().is_empty(), "{outputs}: {:?}", r.calls);
    }
}

/// A flake with a nixConfig is refused, before anything is evaluated.
#[test]
fn a_nix_config_is_refused() {
    let g = Gate::new();
    let d = g.flake_with(
        r#"{ checks.${system}.a = drv "a"; }"#,
        "nixConfig.allow-import-from-derivation = false;",
    );
    let accepting = format!("{}\naccept-flake-config = true", g.nix_config);
    let r = g.run(
        &d,
        Run {
            env: &[("NIX_CONFIG", &accepting)],
            ..Run::default()
        },
    );
    assert!(
        r.fails_with("can't carry this flake's nixConfig"),
        "{}",
        r.out
    );
    assert!(
        r.entries().is_empty() && r.builds().is_empty(),
        "{:?}",
        r.calls
    );
}

/// A malformed output fails it, as nix flake check fails it.
#[test]
fn a_malformed_output_fails_it() {
    let g = Gate::new();
    let system = &g.system;
    for (outputs, error) in [
        (
            "checks = null;",
            "flake output 'checks' is not an attribute set".to_owned(),
        ),
        (
            "formatter = null;",
            "flake output 'formatter' is not an attribute set".to_owned(),
        ),
        (
            "packages = 1;",
            "flake output 'packages' is not an attribute set".to_owned(),
        ),
        (
            "devShells.${system} = null;",
            format!("flake output 'devShells.{system}' is not an attribute set"),
        ),
        (
            "nixosConfigurations = null;",
            "flake output 'nixosConfigurations' is not an attribute set".to_owned(),
        ),
        (
            "legacyPackages = null;",
            "flake output 'legacyPackages' is not an attribute set".to_owned(),
        ),
        (
            "legacyPackages.nodash = { };",
            "'nodash' in flake output 'legacyPackages' is not a valid system type".to_owned(),
        ),
        (
            "packages.nodash = { };",
            "'nodash' in flake output 'packages' is not a valid system type".to_owned(),
        ),
        (
            r#"devShells.nodash = throw "evaluated it";"#,
            "'nodash' in flake output 'devShells' is not a valid system type".to_owned(),
        ),
    ] {
        let base = if outputs.starts_with("packages") {
            r#"checks.${system}.ok = drv "ok";"#
        } else {
            r#"packages.${system}.p = drv "p";"#
        };
        let r = g.apart(&g.flake(&format!("{{ {base} {outputs} }}")));
        assert!(r.fails_with(&error), "{outputs}: {}", r.out);
        assert!(r.entries().is_empty(), "{outputs}: {:?}", r.calls);
    }
}

/// Every check is built under its own name, whatever the name.
#[test]
fn every_check_is_built_under_its_own_name() {
    let g = Gate::new();
    let d = g.flake(
        r#"{
      checks.${system} = {
        a = drv "n0"; "a.b" = drv "n1"; "back\\slash" = drv "n2"; "quo\"te" = drv "n3";
        "sp ace" = drv "n4"; "tab\there" = drv "n5"; "new\nline" = drv "n6"; "$(touch pwned)" = drv "n7";
      };
    }"#,
    );
    for whole in [false, true] {
        let r = g.run(
            &d,
            Run {
                whole,
                skip: "a",
                ..Run::default()
            },
        );
        assert_eq!(r.code, 0, "{}", r.out);
        let b = r.build();
        assert!(!b.contains("-n0.drv"), "{b}");
        for n in 1..8 {
            assert!(b.contains(&format!("-n{n}.drv^*")), "{n}: {b}");
        }
        assert!(!d.join("pwned").exists(), "a name ran as a command");
    }
    let d = g.flake(r#"{ checks.${system} = { "back\\slash" = drv "n2"; ok = drv "ok"; }; }"#);
    let r = g.run(
        &d,
        Run {
            build_fail: "-n2.drv",
            ..Run::default()
        },
    );
    assert_ne!(r.code, 0, "{}", r.out);
}

/// No checks, no build.
#[test]
fn no_checks_no_build() {
    let g = Gate::new();
    let r = g.apart(&g.flake(r#"{ packages.${system}.p = drv "p"; }"#));
    assert!(
        r.code == 0 && r.entries().len() == 1 && r.builds().is_empty(),
        "{} {:?}",
        r.out,
        r.calls
    );
}

/// A check whose first output isn't out is built, every output of it.
#[test]
fn every_output_of_a_check_is_built() {
    let g = Gate::new();
    let d = g.flake(
        r#"{ checks.${system}.multi = derivation {
      name = "multi"; inherit system; builder = "/bin/sh"; outputs = [ "dev" "out" ]; args = [ "-c" "echo > $dev; echo > $out" ];
    }; }"#,
    );
    for whole in [false, true] {
        let r = g.run(
            &d,
            Run {
                whole,
                ..Run::default()
            },
        );
        assert_eq!(r.code, 0, "{}", r.out);
        assert!(r.build().ends_with("-multi.drv^*"));
    }
}
