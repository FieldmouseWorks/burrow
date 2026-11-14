//! keep-builds: each leg restores the builds the last push to the default
//! branch kept for that leg, into a directory there before nix is, whose
//! config names it as a substituter it trusts unsigned, but a PR from a fork
//! restores nothing (its earlier runs could have saved anything under its
//! own ref); and only a push to the default branch keeps them, last, whether
//! its gate passed or failed (#812: a red default branch that kept nothing
//! left every later leg to build cold), under the key and the path restores
//! look for, without its failure failing the gate, with crates/burrow-keep
//! (whose tests say what it keeps) from the workflow's own commit, told how
//! the gate went. A PR's negative run restores
//! every leg's, on the same terms, before installing Nix.

use yaml_serde::Value;

use crate::expr::{Ctx, V, condition, value};
use crate::harness::{repo_file, script, step, steps, text, workflow, yaml};

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
    let (made, restore, install) = (&all[2], &all[4], &all[5]);
    for (keep, nix) in [(true, true), (true, false), (false, true)] {
        let c = ctx(&[
            ("inputs.keep-builds", keep.into()),
            ("inputs.install-nix", nix.into()),
            ("github.event_name", "push".into()),
            // No gate-reuse hit: on one, nothing is restored (reuse.rs).
            ("needs.reuse.outputs.hit", V::Null),
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
            ("needs.reuse.outputs.hit", V::Null),
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
    let exact = "${{ steps.merge.outputs.tip || github.sha }}";
    assert!(
        key.ends_with(&format!("-{exact}"))
            && restore["with"]["restore-keys"] == key.trim_end_matches(exact),
        "{restore:?}"
    );
    assert!(
        key.contains("-leg-${{ matrix.leg }}-of-${{ inputs.legs }}-"),
        "{key}"
    );
    assert!(uses(install, "cachix/install-nix-action@"), "{install:?}");
    assert_eq!(
        install["if"],
        "inputs.install-nix && needs.reuse.outputs.hit != 'true'"
    );
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
    let tool = crate::harness::step(&job, "keep-tool");
    let keep = crate::harness::step(&job, "keep");
    let save = crate::harness::step(&job, "keep-save");
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
            && keep["if"] == "${{ !cancelled() && steps.keep-tool.outcome == 'success' }}"
            && keep["env"]["GATE"] == "${{ steps.gate.outcome }}"
            && keep["continue-on-error"] == true
            && keep["shell"] == "bash"
            && keep["working-directory"] == format!("{dir}/crates")
            && keep["env"]["CARGO_TARGET_DIR"] == "${{ runner.temp }}/keep-build",
        "{keep:?}"
    );
    // Built as the workspace locks it, on the toolchain it pins, and run on
    // the directory the kept builds were restored to and are saved from,
    // told the gate's outcome: a failed gate's evaluation can't say what of
    // the kept builds is stale.
    assert_eq!(
        script(keep),
        "cargo build --release --locked --quiet -p burrow-keep\n\
         \"$CARGO_TARGET_DIR/release/burrow-keep\" --gate \"$GATE\" \"$RUNNER_TEMP\"\n"
    );
    assert!(repo_file("crates/burrow-keep/src/main.rs").is_file());
    assert!(
        save["if"]
            == "${{ !cancelled() && steps.keep.outcome == 'success' && steps.keep-newer.outputs.superseded != 'true' }}"
            && save["continue-on-error"] == true,
        "{save:?}"
    );
    assert!(uses(save, "actions/cache/save@"), "{save:?}");
    assert_eq!(version(save), version(restore));
    // The cache keeps an entry for a restore of the same paths only.
    assert_eq!(save["with"]["path"], restore["with"]["path"]);
    // Under its own commit, the tip a PR merged onto it restores by
    // (a_prs_exact_key_is_its_bases_push_save).
    assert_eq!(
        text(&save["with"]["key"]),
        key.replace(exact, "${{ github.sha }}")
    );
    assert_eq!(save["with"].as_mapping().map(|m| m.len()), Some(2));
    // A red gate keeps its builds too (#812), and the steps after the tool
    // follow it there: each of them would otherwise run only while the job
    // is still green. None runs once the run is cancelled.
    let (main, feat, pr) = ("refs/heads/main", "refs/heads/feat", "refs/pull/12/merge");
    for (event, git_ref, gate, cancelled, kept) in [
        ("push", main, "success", false, true),
        ("push", main, "failure", false, true),
        ("push", main, "failure", true, false),
        ("push", main, "cancelled", false, false),
        ("push", feat, "success", false, false),
        ("push", feat, "failure", false, false),
        ("push", main, "skipped", false, false),
        ("pull_request", pr, "success", false, false),
        ("pull_request", pr, "failure", false, false),
        ("workflow_dispatch", main, "success", false, false),
    ] {
        for (step, id) in [(keep, "keep-tool"), (save, "keep")] {
            // keep-newer said nothing: no newer commit's entry, or it failed.
            let c = ctx(&[
                ("cancelled()", cancelled.into()),
                (&format!("steps.{id}.outcome"), "success".into()),
                ("steps.keep-newer.outputs.superseded", "".into()),
            ]);
            assert_eq!(value(text(&step["if"]), &c).truthy(), !cancelled);
        }
        let mut c = ctx(&[
            ("cancelled()", cancelled.into()),
            ("inputs.keep-builds", true.into()),
            ("inputs.install-nix", true.into()),
            ("steps.gate.outcome", gate.into()),
            ("github.event_name", event.into()),
            ("github.ref", git_ref.into()),
            ("github.event.repository.default_branch", "main".into()),
        ]);
        let case = format!("{event} {git_ref} {gate} {cancelled}");
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
    let (gate_made, gate_restore, gate_install) = (&gate_all[2], &gate_all[4], &gate_all[5]);
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
            // The negative job merges nothing, so it has no merge step's tip.
            ("steps.merge.outputs.tip", "".into()),
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

/// A PR leg that would build cold waits for the base's push to keep the
/// builds, then restores them (#863): only a PR of this repo, only when the
/// restore didn't find the base's own kept builds, before the gate.
#[test]
fn a_pr_leg_waits_for_its_bases_kept_builds_before_the_gate() {
    let job = job();
    let all = steps(&job);
    let at = |id: &str| all.iter().position(|s| s["id"] == id).unwrap();
    assert!(at("kept-restore") < at("kept-wait") && at("kept-wait") < at("kept-rewait"));
    assert!(at("kept-rewait") < at("gate"));
    let wait = &all[at("kept-wait")];
    assert_eq!(wait["continue-on-error"], true);
    let c = |event: &str, head: &str, hit: &str| {
        ctx(&[
            ("inputs.keep-builds", true.into()),
            ("inputs.install-nix", true.into()),
            ("github.event_name", event.into()),
            ("github.repository", "o/r".into()),
            ("github.event.pull_request.head.repo.full_name", head.into()),
            ("steps.kept-restore.outputs.cache-hit", hit.into()),
        ])
    };
    let on = |c: &Ctx| value(text(&wait["if"]), c).truthy();
    assert!(on(&c("pull_request", "o/r", "")));
    assert!(!on(&c("pull_request", "o/r", "true")));
    assert!(!on(&c("pull_request", "someone/r", "")));
    assert!(!on(&c("push", "", "")));
}

/// A PR's exact key is the one its base's push saved under: the merge
/// step's tip, never GitHub's merge commit (github.sha), which no run saves
/// under. Keyed on that, a PR never hit exactly, so the wait's guard on an
/// exact hit never skipped it, and every PR leg planned and restored again
/// what it had already restored (#929). The waiter polls for that same key,
/// and the push's leg under this leg's own job name.
#[test]
fn a_prs_exact_key_is_its_bases_push_save() {
    let job = job();
    let common = [
        ("github.workflow", V::from("flake")),
        ("runner.os", "Linux".into()),
        ("matrix.leg", 2u64.into()),
        ("inputs.legs", 4u64.into()),
    ];
    let with = |more: &[(&str, V)]| {
        let mut c = ctx(&common);
        c.extend(more.iter().map(|(k, v)| ((*k).to_owned(), v.clone())));
        c
    };
    let saved = value(
        text(&step(&job, "keep-save")["with"]["key"]),
        &with(&[("github.sha", "base".into())]),
    )
    .text();
    let pr = with(&[
        ("github.sha", "merge".into()),
        ("steps.merge.outputs.tip", "base".into()),
    ]);
    for id in ["kept-lookup", "kept-restore"] {
        assert_eq!(
            value(text(&step(&job, id)["with"]["key"]), &pr).text(),
            saved,
            "{id}"
        );
        // A push has no merge step: its own commit, as it saves.
        let push = with(&[
            ("github.sha", "base".into()),
            ("steps.merge.outputs.tip", "".into()),
        ]);
        assert_eq!(
            value(text(&step(&job, id)["with"]["key"]), &push).text(),
            saved,
            "{id}"
        );
    }
    let env = &step(&job, "kept-wait")["env"];
    let waits_for = format!(
        "{}{}",
        value(text(&env["KEPT_KEY"]), &pr).text(),
        value(text(&env["TIP"]), &pr).text()
    );
    assert_eq!(waits_for, saved);
    assert_eq!(env["LEG_JOB"], job["name"]);
}

#[test]
fn the_waiters_and_pruners_token_has_actions_access_through_every_local_caller() {
    let check = job();
    assert!(
        check.get("permissions").is_none(),
        "inherit the caller token, including optional actions access"
    );
    assert_eq!(
        step(&check, "kept-wait")["env"]["GH_TOKEN"],
        "${{ github.token }}"
    );
    for path in [
        ".github/workflows/flake.yml",
        ".github/workflows/flake-retarget.yml",
        "templates/project/.github/workflows/flake.yml",
        "templates/project/.github/workflows/flake-retarget.yml",
    ] {
        let wf = workflow(path);
        for (name, job) in wf["jobs"].as_mapping().unwrap() {
            if job["uses"]
                .as_str()
                .is_some_and(|u| u.contains("/flake-check.yml"))
            {
                // burrow's own push to main prunes the kept builds (#1011),
                // and its other calls grant what that one does.
                let actions = if path.starts_with(".github/") {
                    "write"
                } else {
                    "read"
                };
                for (scope, level) in [
                    ("actions", actions),
                    ("contents", "read"),
                    ("id-token", "write"),
                    ("attestations", "write"),
                ] {
                    assert_eq!(job["permissions"][scope], level, "{path} {name:?}: {scope}");
                }
            }
        }
    }
}

/// One entry a leg (#1011): a push to the default branch's leg checks for a
/// newer commit's entry before its save, and saves only without one; once
/// its save is listed it deletes what its commit supersedes, with
/// burrow-keep-prune (whose tests say which), built by the keep step, and
/// the caller's token. Either failing is a degradation, not a red gate.
#[test]
fn a_leg_skips_a_superseded_save_and_prunes_what_its_own_supersedes() {
    let job = job();
    let all = steps(&job);
    let at = |id: &str| all.iter().position(|s| s["id"] == id).unwrap();
    assert!(at("keep") < at("keep-newer") && at("keep-newer") < at("keep-save"));
    assert!(at("saved-lookup") < at("keep-prune"));
    let (newer, save, prune) = (
        step(&job, "keep-newer"),
        step(&job, "keep-save"),
        step(&job, "keep-prune"),
    );
    let kept_key = text(&save["with"]["key"]).trim_end_matches("${{ github.sha }}");
    for (s, mode) in [(newer, "before"), (prune, "after")] {
        assert_eq!(s["continue-on-error"], true, "{s:?}");
        assert_eq!(s["shell"], "bash");
        assert_eq!(script(s), format!("\"$PRUNE\" {mode}"));
        assert_eq!(s["env"]["GH_TOKEN"], "${{ github.token }}");
        assert_eq!(s["env"]["KEPT_KEY"], kept_key);
        assert_eq!(
            s["env"]["DEFAULT_BRANCH"],
            "${{ github.event.repository.default_branch }}"
        );
        // The binary the keep step's cargo build left in its target dir.
        let keep = step(&job, "keep");
        assert_eq!(
            s["env"]["PRUNE"],
            format!(
                "{}/release/burrow-keep-prune",
                text(&keep["env"]["CARGO_TARGET_DIR"])
            )
        );
    }
    assert!(repo_file("crates/burrow-keep/src/bin/burrow-keep-prune.rs").is_file());
    // Saved unless a newer commit's entry was there, or the run cancelled.
    for (cancelled, kept, superseded, saves) in [
        (false, "success", "", true),
        (false, "success", "true", false),
        (false, "failure", "", false),
        (true, "success", "", false),
    ] {
        let c = ctx(&[
            ("cancelled()", cancelled.into()),
            ("steps.keep.outcome", kept.into()),
            ("steps.keep-newer.outputs.superseded", superseded.into()),
        ]);
        assert_eq!(value(text(&save["if"]), &c).truthy(), saves, "{c:?}");
        assert_eq!(
            value(text(&newer["if"]), &c).truthy(),
            !cancelled && kept == "success"
        );
    }
    // Pruned only once this push's own entry exists: never its
    // predecessor without a successor.
    for (cancelled, hit, prunes) in [
        (false, "true", true),
        (false, "false", false),
        (false, "", false),
        (true, "true", false),
    ] {
        let c = ctx(&[
            ("cancelled()", cancelled.into()),
            ("steps.saved-lookup.outputs.cache-hit", hit.into()),
        ]);
        assert_eq!(value(text(&prune["if"]), &c).truthy(), prunes, "{c:?}");
    }
}
