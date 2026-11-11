//! keep-builds: each leg restores the builds the last push to the default
//! branch kept for that leg, into a directory there before nix is, whose
//! config names it as a substituter it trusts unsigned, but a PR from a fork
//! restores nothing (its earlier runs could have saved anything under its
//! own ref); and only a push to the default branch whose gate passed keeps
//! them, last, under the key and the path restores look for, without its
//! failure failing the gate, with crates/burrow-keep (whose tests say what
//! it keeps) from the workflow's own commit. A PR's negative run restores
//! every leg's, on the same terms, before installing Nix.

use yaml_serde::Value;

use crate::expr::{Ctx, V, condition, value};
use crate::harness::{repo_file, script, steps, text, workflow, yaml};

fn job() -> Value {
    workflow(".github/workflows/flake-check.yml")["jobs"]["check"].clone()
}

fn ctx(pairs: &[(&str, V)]) -> Ctx {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// The steps that restore the kept builds and let nix take them, and the
/// three that keep them, last.
#[test]
fn every_run_but_a_forks_restores_and_only_a_default_branch_push_keeps() {
    let job = job();
    let all = steps(&job);
    let (made, restore, install) = (&all[2], &all[3], &all[4]);
    for (keep, nix) in [(true, true), (true, false), (false, true)] {
        let c = ctx(&[
            ("inputs.keep-builds", keep.into()),
            ("inputs.install-nix", nix.into()),
            ("github.event_name", "push".into()),
        ]);
        assert_eq!(condition(text(&made["if"]), &c), keep && nix, "{made:?}");
        assert_eq!(value(text(&restore["if"]), &c).truthy(), keep && nix);
    }
    // A PR's own earlier runs can save under its ref, which its later runs
    // restore from first: one from a fork, anyone's, restores nothing, nor
    // one whose fork is gone (GitHub gives its head no repo).
    for (event, head, restores) in [
        ("push", None, true),
        ("workflow_dispatch", None, true),
        ("pull_request", Some("FieldmouseWorks/burrow"), true),
        ("pull_request", Some("someone/burrow"), false),
        ("pull_request", None, false),
    ] {
        let mut c = ctx(&[
            ("inputs.keep-builds", true.into()),
            ("inputs.install-nix", true.into()),
            ("github.event_name", event.into()),
            ("github.repository", "FieldmouseWorks/burrow".into()),
        ]);
        if event == "pull_request" {
            c.insert(
                "github.event.pull_request.head.repo.full_name".to_owned(),
                head.map_or(V::Null, Into::into),
            );
        }
        let case = format!("{event} {head:?}");
        assert_eq!(value(text(&restore["if"]), &c).truthy(), restores, "{case}");
        // The directory nix's config names is there either way.
        assert!(condition(text(&made["if"]), &c), "{case}");
    }
    assert_eq!(script(made), "mkdir -p \"$RUNNER_TEMP/kept\"");
    let uses = |s: &Value, action: &str| text(&s["uses"]).starts_with(action);
    assert!(uses(restore, "actions/cache/restore@"), "{restore:?}");
    assert_eq!(restore["with"]["path"], "${{ runner.temp }}/kept");
    let key = text(&restore["with"]["key"]);
    assert!(
        key.ends_with("-${{ github.sha }}")
            && restore["with"]["restore-keys"] == key.trim_end_matches("${{ github.sha }}"),
        "{restore:?}"
    );
    assert!(
        key.contains("-leg-${{ matrix.leg }}-of-${{ inputs.legs }}-"),
        "{key}"
    );
    assert!(uses(install, "cachix/install-nix-action@"), "{install:?}");
    assert_eq!(install["if"], "inputs.install-nix");
    for (keep, said) in [
        (
            true,
            "extra-substituters = file:///tmp/runner/kept?trusted=true",
        ),
        (false, ""),
    ] {
        let c = ctx(&[
            ("inputs.keep-builds", keep.into()),
            ("runner.temp", "/tmp/runner".into()),
        ]);
        assert_eq!(
            value(text(&install["with"]["extra_nix_config"]), &c).text(),
            said
        );
    }
    let [tool, keep, save] = &all[all.len() - 3..] else {
        unreachable!()
    };
    // burrow-keep from the commit this workflow is, whichever repo calls it,
    // with nothing that can push.
    assert!(
        tool["id"] == "keep-tool" && tool["continue-on-error"] == true,
        "{tool:?}"
    );
    let version = |s: &Value| text(&s["uses"]).split_once('@').map(|(_, v)| v.to_owned());
    assert!(uses(tool, "actions/checkout@"), "{tool:?}");
    assert_eq!(version(tool), version(&all[0]));
    let dir = ".flake-check-keep";
    assert_eq!(
        tool["with"],
        yaml(&format!(
            "repository: ${{{{ job.workflow_repository }}}}\n\
             ref: ${{{{ job.workflow_sha }}}}\n\
             path: {dir}\n\
             persist-credentials: false\n\
             sparse-checkout: crates\n"
        ))
    );
    assert!(
        keep["id"] == "keep"
            && keep["if"] == "steps.keep-tool.outcome == 'success'"
            && keep["continue-on-error"] == true
            && keep["shell"] == "bash"
            && keep["working-directory"] == format!("{dir}/crates")
            && keep["env"]["CARGO_TARGET_DIR"] == "${{ runner.temp }}/keep-build",
        "{keep:?}"
    );
    // Built as the workspace locks it, on the toolchain it pins, and run on
    // the directory the kept builds were restored to and are saved from.
    assert_eq!(
        script(keep),
        "cargo build --release --locked --quiet -p burrow-keep\n\
         \"$CARGO_TARGET_DIR/release/burrow-keep\" \"$RUNNER_TEMP\"\n"
    );
    assert!(repo_file("crates/burrow-keep/src/main.rs").is_file());
    assert!(
        save["if"] == "steps.keep.outcome == 'success'" && save["continue-on-error"] == true,
        "{save:?}"
    );
    assert!(uses(save, "actions/cache/save@"), "{save:?}");
    assert_eq!(version(save), version(restore));
    // The cache keeps an entry for a restore of the same paths only.
    assert_eq!(save["with"]["path"], restore["with"]["path"]);
    assert_eq!(save["with"]["key"], key);
    assert_eq!(save["with"].as_mapping().map(|m| m.len()), Some(2));
    for (event, git_ref, gate, kept) in [
        ("push", "refs/heads/main", "success", true),
        ("push", "refs/heads/main", "failure", false),
        ("push", "refs/heads/feat", "success", false),
        ("push", "refs/heads/main", "skipped", false),
        ("pull_request", "refs/pull/12/merge", "success", false),
        ("workflow_dispatch", "refs/heads/main", "success", false),
    ] {
        let mut c = ctx(&[
            ("inputs.keep-builds", true.into()),
            ("inputs.install-nix", true.into()),
            ("steps.gate.outcome", gate.into()),
            ("github.event_name", event.into()),
            ("github.ref", git_ref.into()),
            ("github.event.repository.default_branch", "main".into()),
        ]);
        let case = format!("{event} {git_ref} {gate}");
        assert_eq!(value(text(&tool["if"]), &c).truthy(), kept, "{case}");
        for off in ["inputs.keep-builds", "inputs.install-nix"] {
            c.insert(off.to_owned(), false.into());
            assert!(!value(text(&tool["if"]), &c).truthy(), "{case} {off}");
            c.insert(off.to_owned(), true.into());
        }
    }
}

/// The negative job restores every leg's kept builds, under the keys and
/// path each leg restores its own from and on the same terms (no PR from a
/// fork), before installing Nix, and nix substitutes from them as the
/// legs' does. Without them it built what no public cache
/// has (the workspace's dependencies, the machines' patched packages) again
/// for every PR that adds a test.
#[test]
fn a_negative_restores_every_legs_kept_builds() {
    let wf = workflow(".github/workflows/flake-check.yml");
    let neg = &wf["jobs"]["negative"];
    let all = steps(neg);
    let gate = job();
    let gate_all = steps(&gate);
    let (gate_made, gate_restore, gate_install) = (&gate_all[2], &gate_all[3], &gate_all[4]);
    let uses = |s: &Value, action: &str| s["uses"].as_str().is_some_and(|u| u.starts_with(action));
    let at = |f: &dyn Fn(&Value) -> bool| all.iter().position(f).unwrap();
    let restores: Vec<&Value> = all
        .iter()
        .filter(|s| uses(s, "actions/cache/restore@") && s["with"]["lookup-only"] != true)
        .collect();
    assert_eq!(restores.len(), 8, "one a leg, up to eight");
    let first = at(&|s| uses(s, "actions/cache/restore@"));
    let made = &all[at(&|s| s.get("run").is_some_and(|r| r == script(gate_made)))];
    let install = &all[at(&|s| uses(s, "cachix/install-nix-action@"))];
    // Restores must precede Nix opening the file cache, including the
    // paths-only bootstrap. This fails on the order introduced by #545.
    let install_at = at(&|s| uses(s, "cachix/install-nix-action@"));
    let paths_at = at(&|s| s["id"] == "paths");
    assert!(at(&|s| s.get("run").is_some_and(|r| r == script(gate_made))) < first);
    for restore in &restores {
        assert!(at(&|s| s == *restore) < install_at);
    }
    assert!(install_at < paths_at);
    assert!(paths_at < at(&|s| s["id"] == "negative"));
    assert_eq!(install["if"], "inputs.install-nix");
    assert_eq!(script(made), script(gate_made));
    let base = |legs: u64| {
        ctx(&[
            ("inputs.keep-builds", true.into()),
            ("inputs.install-nix", true.into()),
            ("inputs.legs", legs.into()),
            ("steps.paths.outputs.needs", "true".into()),
            ("github.event_name", "pull_request".into()),
            ("github.repository", "FieldmouseWorks/burrow".into()),
            (
                "github.event.pull_request.head.repo.full_name",
                "FieldmouseWorks/burrow".into(),
            ),
            ("github.workflow", "flake".into()),
            ("runner.os", "Linux".into()),
            ("runner.temp", "/tmp/runner".into()),
            ("github.sha", "abc".into()),
        ])
    };
    for legs in 1..=8u64 {
        for (k, r) in restores.iter().enumerate() {
            let leg = k as u64 + 1;
            let c = base(legs);
            assert_eq!(
                value(text(&r["if"]), &c).truthy(),
                leg <= legs,
                "leg {leg} of {legs}"
            );
            // The entry that leg's own restore finds.
            let mut as_leg = c.clone();
            as_leg.insert("matrix.leg".to_owned(), leg.into());
            for field in ["path", "key", "restore-keys"] {
                assert_eq!(
                    value(text(&r["with"][field]), &c).text(),
                    value(text(&gate_restore["with"][field]), &as_leg).text(),
                    "leg {leg} of {legs}: {field}"
                );
            }
            assert_eq!(r["uses"], gate_restore["uses"]);
        }
    }
    // None for a PR from a fork, or one whose fork is gone, nor with
    // keep-builds or nix's install off.
    for (key, off) in [
        (
            "github.event.pull_request.head.repo.full_name",
            V::from("someone/burrow"),
        ),
        ("github.event.pull_request.head.repo.full_name", V::Null),
        ("inputs.keep-builds", false.into()),
        ("inputs.install-nix", false.into()),
    ] {
        let mut c = base(8);
        c.insert(key.to_owned(), off.clone());
        for r in &restores {
            assert!(!value(text(&r["if"]), &c).truthy(), "{key} {off:?}");
        }
        if key.starts_with("inputs.") {
            assert!(!condition(text(&made["if"]), &c), "{key} {off:?}");
        }
    }
    assert!(condition(text(&made["if"]), &base(1)));
    assert_eq!(
        install["with"]["extra_nix_config"],
        gate_install["with"]["extra_nix_config"]
    );
    // Lookup does not extract. Both an exact and a prefix hit require a
    // successful restore; a genuine miss may build cold on first use.
    for (k, restore) in restores.iter().enumerate() {
        let id = format!("kept-{}", k + 1);
        let lookup = &all[at(&|s| s["id"] == id)];
        assert!(at(&|s| s == lookup) < at(&|s| s == *restore));
        assert_eq!(lookup["if"], restore["if"]);
        assert_eq!(lookup["uses"], restore["uses"]);
        assert_eq!(lookup["with"]["lookup-only"], true);
        for field in ["path", "key", "restore-keys"] {
            assert_eq!(lookup["with"][field], restore["with"][field]);
        }
        for (hit, required) in [("true", true), ("false", true), ("", false)] {
            let c = ctx(&[(&format!("steps.{id}.outputs.cache-hit"), hit.into())]);
            assert_eq!(
                value(text(&restore["with"]["fail-on-cache-miss"]), &c).truthy(),
                required,
                "{id}: {hit}"
            );
        }
    }
}
