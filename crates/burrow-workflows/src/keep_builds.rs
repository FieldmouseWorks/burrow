//! keep-builds: each leg restores the builds the last push to the default
//! branch kept for that leg, into a directory there before nix is, whose
//! config names it as a substituter it trusts unsigned, but a PR from a fork
//! restores nothing (its earlier runs could have saved anything under its
//! own ref); and only a push to the default branch whose gate passed keeps
//! them, last, under the key and the path restores look for, without its
//! failure failing the gate, with crates/burrow-keep (whose tests say what
//! it keeps) from the workflow's own commit.

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
