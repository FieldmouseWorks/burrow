//! The gate split across legs: each leg makes the whole plan, evaluates every
//! Nth output from its own on, and builds only the checks among them, so the
//! legs together evaluate each output once and build each check once.

use std::collections::BTreeSet;
use std::fs;

use super::{EVERY, Gate, Run};
use crate::harness::read;

/// With legs, each leg evaluates every Nth output from its own on, and
/// builds only its checks: together, each once.
#[test]
fn each_leg_takes_its_own_share() {
    let g = Gate::new();
    let d = g.flake(
        r#"{
      checks.${system} = { a = drv "a"; b = drv "b"; c = drv "c"; d = drv "d"; vm = drv "vm"; };
      packages.${system}.p = drv "p";
      nixosConfigurations.box.config.system.build.toplevel = drv "box";
    }"#,
    );
    let r = g.run(
        &d,
        Run {
            skip: "vm",
            ..Run::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.out);
    let mut whole: Vec<String> = r.entries().into_iter().map(str::to_owned).collect();
    assert_eq!(whole.len(), 7, "{:?}", r.calls);
    whole.sort();
    let plan = g.s.join("plan.json");
    for n in [1, 2, 3, 7, 8] {
        let (mut seen, mut built, mut plans) = (Vec::new(), Vec::new(), BTreeSet::new());
        for k in 1..=n {
            let _ = fs::remove_file(&plan);
            let (leg, legs) = (k.to_string(), n.to_string());
            let r = g.run(
                &d,
                Run {
                    skip: "vm",
                    legs: Some((&leg, &legs)),
                    ..Run::default()
                },
            );
            assert_eq!(r.code, 0, "leg {k} of {n}: {}", r.out);
            // The plan it shares out, where the leg's step reads it for the
            // gate job to hold it to every other leg's.
            plans.insert(read(&plan));
            // Every leg makes the plan, then evaluates its own share.
            assert_eq!(r.calls[0], "eval plan", "leg {k} of {n}: {:?}", r.calls);
            let want: Vec<String> = (k - 1..7)
                .step_by(n)
                .map(|i| format!("eval entry {i}"))
                .collect();
            assert_eq!(r.entries(), want, "leg {k} of {n}: {:?}", r.calls);
            assert_eq!(
                r.out.contains(&format!("leg {k} of {n}:")),
                n > 1,
                "leg {k} of {n}: {}",
                r.out
            );
            seen.extend(r.entries().into_iter().map(str::to_owned));
            for b in r.builds() {
                built.extend(b.split_whitespace().skip(4).map(|t| {
                    t.split_once('-')
                        .unwrap_or_else(|| panic!("{t}"))
                        .1
                        .to_owned()
                }));
            }
        }
        seen.sort();
        assert_eq!(seen, whole, "{n} legs");
        assert_eq!(plans.len(), 1, "{n} legs: {plans:?}");
        // Each check built once, by the leg that evaluated it, and the
        // skipped one by none.
        built.sort();
        assert_eq!(
            built,
            ["a.drv^*", "b.drv^*", "c.drv^*", "d.drv^*"],
            "{n} legs"
        );
    }
    // A failure in one leg fails that leg, and only that one.
    let codes: Vec<i32> = ["1", "2", "3"]
        .into_iter()
        .map(|k| {
            g.run(
                &d,
                Run {
                    skip: "vm",
                    build_fail: "-c.drv",
                    legs: Some((k, "3")),
                    ..Run::default()
                },
            )
            .code
        })
        .collect();
    assert!(codes[2] != 0 && codes[..2] == [0, 0], "{codes:?}");
    let broken = g.flake(r#"{ checks.${system} = { a = drv "a"; b = throw "broken check"; }; }"#);
    let first = g.run(
        &broken,
        Run {
            legs: Some(("1", "2")),
            ..Run::default()
        },
    );
    assert_eq!(first.code, 0, "{}", first.out);
    let second = g.run(
        &broken,
        Run {
            legs: Some(("2", "2")),
            ..Run::default()
        },
    );
    assert!(second.fails_with("broken check"), "{}", second.out);
}

/// Legs without eval-apart, or a leg that isn't one of 1 to 8, are refused
/// before anything is evaluated.
#[test]
fn legs_it_cant_run_are_refused() {
    let g = Gate::new();
    let every = g.flake(EVERY);
    for (apart, leg, legs, why) in [
        (false, "1", "2", "legs need eval-apart"),
        (true, "3", "2", "can't run leg 3 of 2"),
        (true, "1", "9", "can't run leg 1 of 9"),
        (true, "0", "1", "can't run leg 0 of 1"),
        (true, "", "", "can't run leg  of"),
        (true, "1", "x", "can't run leg 1 of x"),
    ] {
        let r = g.run(
            &every,
            Run {
                whole: !apart,
                legs: Some((leg, legs)),
                ..Run::default()
            },
        );
        assert!(r.fails_with(why), "{apart} {leg} {legs}: {}", r.out);
        assert!(r.calls.is_empty(), "{apart} {leg} {legs}: {:?}", r.calls);
    }
}
