//! Planning is not a gate run: it never enters a check build or a fallback.
use super::{EVERY, Gate, Run};
#[test]
fn planning_stops_before_every_build_path_and_the_gate_still_builds() {
    let g = Gate::new();
    let normal = g.flake(EVERY);
    let configured = g.flake_with(EVERY, r#"nixConfig.bash-prompt = "x";"#);
    let ifd = g.flake(
        r#"{ checks.${system}.ifd = builtins.seq (builtins.readFile (drv "ifd")) (drv "check"); }"#,
    );
    let broken =
        g.flake(r#"{ checks.${system} = { a = drv "a"; broken = throw "broken check"; }; }"#);
    let broken_plan = g.flake(r#"{ checks = throw "broken plan"; }"#);
    for whole in [true, false] {
        for skip in ["", "vm"] {
            for d in [&normal, &configured, &broken, &broken_plan, &ifd] {
                let r = Run {
                    whole,
                    skip,
                    ..Run::default()
                };
                let (_, env) = g.gate_step(&r);
                let env: Vec<(&str, &str)> =
                    env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
                let ran = g.s.bash(r#"bash "$TOOLS/run.sh" cold"#, d, &env);
                let calls = g.calls();
                assert!(
                    calls
                        .iter()
                        .all(|c| !c.starts_with("build ") && !c.starts_with("flake check")),
                    "{whole} {skip} {d:?}: {calls:?}\n{}",
                    ran.said()
                );
                if d == &normal {
                    assert_eq!(ran.code, 0, "{}", ran.said());
                    let cold = crate::harness::read(&g.s.join("cold.txt"));
                    let expected = if skip.is_empty() { "a\nvm" } else { "a" };
                    assert_eq!(cold, expected, "{whole} {skip}: {}", ran.said());
                    let gate = g.run(
                        d,
                        Run {
                            whole,
                            skip,
                            env: &[("COLD_ONLY", "1")],
                            ..Run::default()
                        },
                    );
                    assert_eq!(gate.code, 0, "{}", gate.out);
                    assert!(
                        !gate.builds().is_empty(),
                        "{whole} {skip}: {:?}",
                        gate.calls
                    );
                    assert!(!gate.build().contains("-p.drv"));
                    if !skip.is_empty() {
                        assert!(!gate.build().contains("-vm.drv"));
                    }
                } else {
                    assert_ne!(ran.code, 0, "{}", ran.said());
                    if d == &ifd {
                        assert!(
                            ran.said().contains("allow-import-from-derivation"),
                            "{}",
                            ran.said()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_wait_bootstrap_builds_only_the_api_binary_with_the_pinned_compiler() {
    let g = Gate::new();
    let dir = g.flake("{}");
    crate::harness::executable(
        &g.s.join("compiler/bin/cargo"),
        r#"
[ "$PWD" != "$CALLER" ] && [ "${PWD%/*}" = "$RUNNER_TEMP" ] || exit 90
[ "$RUSTC" = "$PINNED_RUSTC" ] && [ -z "$RUSTC_WRAPPER" ] || exit 91
[ "$1 $2 $3" = 'build --locked --quiet' ] || exit 92
shift 3
[ "$1" = --manifest-path ] && [ "$2" -ef "$TOOLS/../../crates/Cargo.toml" ] || exit 93
shift 2
[ "$1" = --target-dir ] || exit 94
target=$2
shift 2
[ "$*" = '-p burrow-flake-check --bin burrow-flake-wait' ] || exit 95
mkdir -p "$target/debug"
printf '#!/usr/bin/env bash\necho api-waiter-ran\n' > "$target/debug/burrow-flake-wait"
chmod +x "$target/debug/burrow-flake-wait"
"#,
    );
    let pinned = g.s.join("compiler/bin/rustc");
    let (_, env) = g.gate_step(&Run::default());
    let mut env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    env.extend([
        ("CALLER", dir.to_str().unwrap()),
        ("PINNED_RUSTC", pinned.to_str().unwrap()),
    ]);
    let ran = g.s.bash(r#"bash "$TOOLS/run.sh" wait"#, &dir, &env);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(ran.said().contains("api-waiter-ran"), "{}", ran.said());
}
