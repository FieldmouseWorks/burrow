//! The gate split across legs: each leg makes the whole plan, evaluates the
//! outputs whose names hash to it, and builds only the checks among them, so
//! the legs together evaluate each output once and build each check once.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use super::{EVERY, Gate, Run};
use crate::harness::read;

/// With legs, each leg evaluates its own share of the outputs, said in the
/// plan's order, and builds only its checks: together, each once.
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
            // Each said in the plan's order, however they ran.
            let plan_json: serde_json::Value = serde_json::from_str(&read(&plan)).unwrap();
            let order: Vec<&str> = plan_json["entries"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["what"].as_str().unwrap())
                .collect();
            let said: Vec<usize> = r
                .out
                .lines()
                .filter_map(|l| l.strip_prefix("evaluating "))
                .map(|w| order.iter().position(|o| *o == w).unwrap())
                .collect();
            assert_eq!(said.len(), r.entries().len(), "leg {k} of {n}: {}", r.out);
            assert!(said.is_sorted(), "leg {k} of {n}: {}", r.out);
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
    let c = legs_of(&g, &d, 3)[&format!("checks.{}.c", g.system)];
    let failed: Vec<usize> = (1..=3)
        .filter(|k| {
            g.run(
                &d,
                Run {
                    skip: "vm",
                    build_fail: "-c.drv",
                    legs: Some((&k.to_string(), "3")),
                    ..Run::default()
                },
            )
            .code
                != 0
        })
        .collect();
    assert_eq!(failed, [c]);
    let broken = g.flake(r#"{ checks.${system} = { a = drv "a"; b = throw "broken check"; }; }"#);
    let runs: Vec<_> = ["1", "2"]
        .into_iter()
        .map(|k| {
            g.run(
                &broken,
                Run {
                    legs: Some((k, "2")),
                    ..Run::default()
                },
            )
        })
        .collect();
    let failed: Vec<_> = runs.iter().filter(|r| r.code != 0).collect();
    assert!(
        failed.len() == 1 && failed[0].fails_with("broken check"),
        "{:?}",
        runs.iter().map(|r| &r.out).collect::<Vec<_>>()
    );
}

/// Each output's leg is its name's: one added that sorts first, ahead of
/// every other in the plan, moves none of them to another leg, away from
/// the builds main's gate kept for it there (keep-builds).
#[test]
fn an_output_added_moves_no_other_to_another_leg() {
    let g = Gate::new();
    let checks = r#"a = drv "a"; b = drv "b"; c = drv "c"; d = drv "d"; e = drv "e"; f = drv "f";"#;
    let before = g.flake(&format!(
        r#"{{ checks.${{system}} = {{ {checks} }}; packages.${{system}}.p = drv "p"; }}"#
    ));
    let after = g.flake(&format!(
        r#"{{ checks.${{system}} = {{ "0" = drv "zero"; {checks} }}; packages.${{system}}.p = drv "p"; }}"#
    ));
    for n in [2, 3, 4, 8] {
        let (was, now) = (legs_of(&g, &before, n), legs_of(&g, &after, n));
        assert_eq!(now.len(), was.len() + 1, "{n} legs: {now:?}");
        // Shared out, not all one leg's.
        assert!(
            was.values().collect::<BTreeSet<_>>().len() > 1,
            "{n} legs: {was:?}"
        );
        for (what, leg) in &was {
            assert_eq!(
                now.get(what),
                Some(leg),
                "{n} legs: {what} moved: {was:?} then {now:?}"
            );
        }
    }
}

/// Which leg of `n` evaluates each of the flake's outputs, by name.
fn legs_of(g: &Gate, d: &Path, n: usize) -> BTreeMap<String, usize> {
    let mut legs = BTreeMap::new();
    for k in 1..=n {
        let (leg, of) = (k.to_string(), n.to_string());
        let r = g.run(
            d,
            Run {
                legs: Some((&leg, &of)),
                ..Run::default()
            },
        );
        assert_eq!(r.code, 0, "leg {k} of {n}: {}", r.out);
        let plan: serde_json::Value = serde_json::from_str(&read(&g.s.join("plan.json"))).unwrap();
        for e in r.entries() {
            let i: usize = e.strip_prefix("eval entry ").unwrap().parse().unwrap();
            let what = plan["entries"][i]["what"].as_str().unwrap().to_owned();
            assert_eq!(legs.insert(what, k), None, "leg {k} of {n}: {e} twice");
        }
    }
    legs
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
