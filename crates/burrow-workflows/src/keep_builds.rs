//! keep-builds: each leg restores the builds the last push to the default
//! branch kept for that leg, into a directory there before nix is, whose
//! config names it as a substituter it trusts unsigned; and only a push to
//! the default branch whose gate passed keeps them, last, under the key
//! restores look for, without its failure failing the gate. What it keeps is
//! every path in the store no one signed (built here, or taken from the kept
//! builds), but derivations; and what the kept builds hold that a derivation
//! here still makes, though this gate didn't need it (what an unchanged check
//! was built from), but nothing no derivation here makes.

use yaml_serde::Value;

use crate::expr::{Ctx, V, condition, value};
use crate::harness::{Scratch, read, script, steps, text, workflow, write};

fn job() -> Value {
    workflow(".github/workflows/flake-check.yml")["jobs"]["check"].clone()
}

fn ctx(pairs: &[(&str, V)]) -> Ctx {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

/// The steps that restore the kept builds and let nix take them, and the two
/// that keep them, last.
#[test]
fn every_run_restores_and_only_a_default_branch_push_keeps() {
    let job = job();
    let all = steps(&job);
    let (made, restore, install) = (&all[2], &all[3], &all[4]);
    for (keep, nix) in [(true, true), (true, false), (false, true)] {
        let c = ctx(&[
            ("inputs.keep-builds", keep.into()),
            ("inputs.install-nix", nix.into()),
        ]);
        for s in [made, restore] {
            assert_eq!(condition(text(&s["if"]), &c), keep && nix, "{s:?}");
        }
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
    let (keep, save) = (&all[all.len() - 2], &all[all.len() - 1]);
    assert!(
        keep["id"] == "keep"
            && keep["continue-on-error"] == true
            && keep["shell"] == "bash"
            && keep["working-directory"] == "${{ runner.temp }}",
        "{keep:?}"
    );
    assert!(
        save["if"] == "steps.keep.outcome == 'success'" && save["continue-on-error"] == true,
        "{save:?}"
    );
    assert!(uses(save, "actions/cache/save@"), "{save:?}");
    let version = |s: &Value| text(&s["uses"]).split_once('@').map(|(_, v)| v.to_owned());
    assert_eq!(version(save), version(restore));
    assert_eq!(save["with"]["path"], "${{ runner.temp }}/keep");
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
        assert_eq!(value(text(&keep["if"]), &c).truthy(), kept, "{case}");
        for off in ["inputs.keep-builds", "inputs.install-nix"] {
            c.insert(off.to_owned(), false.into());
            assert!(!value(text(&keep["if"]), &c).truthy(), "{case} {off}");
            c.insert(off.to_owned(), true.into());
        }
    }
}

const STORE: &str = "/nix/store/";

/// A default-branch push keeps what no one signed but derivations, and what
/// its derivations were built from: each path on its own, from the store as
/// nix 2.35 writes it, with stand-ins for nix that take only the calls the
/// step makes.
#[test]
fn a_default_branch_push_keeps_what_no_one_signed_and_what_it_was_built_from() {
    let s = Scratch::default();
    s.stub(
        "nix",
        r#"kept="file://$RUNNER_TEMP/kept" keep="file://$RUNNER_TEMP/keep?compression=none"
case "$*" in
  "path-info --all --json --json-format 2") cat "$PATHINFO" ;;
  "path-info --all --store $kept") cat "$KEPT" ;;
  "copy --no-recursive --to $keep "*) shift 4; echo "store $*" >> "$COPIED" ;;
  "copy --no-recursive --from $kept?trusted=true --to $keep "*) shift 6; echo "kept $*" >> "$COPIED" ;;
  *) echo "nix $*: not stood in for" >&2; exit 1 ;;
esac
"#,
    );
    s.stub(
        "nix-store",
        r#"[ "$1 $2" = "--query --outputs" ] || { echo "nix-store $*: not stood in for" >&2; exit 1; }
shift 2
for drv; do grep "^$drv " "$OUTPUTS" | cut -d' ' -f2- | tr ' ' '\n' | grep . || exit 1; done
"#,
    );
    // Built here: git and a VM test. Signed: hello, from a public cache.
    // Taken from the kept builds: a check whose derivation hadn't changed.
    // Kept before but not needed: tmux, which a derivation here still makes,
    // and an old check, which none does.
    let info: serde_json::Map<String, serde_json::Value> = [
        ("aaa-git-2.54", vec![]),
        ("bbb-vm-test-run-proj", vec![]),
        ("ccc-hello", vec!["cache.nixos.org-1:x"]),
        ("ddd-gate.drv", vec![]),
        ("eee-kept-before", vec![]),
        ("fff-git.drv", vec![]),
        ("ggg-tmux.drv", vec![]),
        ("jjj-hello.drv", vec![]),
        ("kkk-check.drv", vec![]),
    ]
    .into_iter()
    .map(|(p, sigs)| {
        (
            p.to_owned(),
            serde_json::json!({"signatures": sigs, "version": 2}),
        )
    })
    .collect();
    let outputs = [
        ("ddd-gate.drv", "bbb-vm-test-run-proj"),
        ("fff-git.drv", "aaa-git-2.54"),
        ("ggg-tmux.drv", "hhh-tmux hhh-tmux-man"),
        ("jjj-hello.drv", "ccc-hello"),
        ("kkk-check.drv", "eee-kept-before"),
    ];
    let runner = s.mkdtemp();
    let file = |name: &str| runner.join(name).to_str().unwrap().to_owned();
    write(
        &runner.join("info.json"),
        &serde_json::json!({"info": info, "storeDir": "/nix/store", "version": 2}).to_string(),
    );
    let in_store = |names: &str| -> String {
        names
            .split(' ')
            .map(|n| format!("{STORE}{n}"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    write(
        &runner.join("outputs"),
        &outputs
            .iter()
            .map(|(d, outs)| format!("{STORE}{d} {}\n", in_store(outs)))
            .collect::<String>(),
    );
    // In no order: the step sorts them.
    write(
        &runner.join("kept.txt"),
        &[
            "zzz-old-check",
            "hhh-tmux",
            "eee-kept-before",
            "hhh-tmux-man",
        ]
        .iter()
        .map(|p| format!("{STORE}{p}\n"))
        .collect::<String>(),
    );
    std::fs::create_dir(runner.join("kept")).unwrap();
    let keep = steps(&job())
        .iter()
        .find(|s| s["id"] == "keep")
        .expect("a keep step")
        .clone();
    let ran = s.bash(
        script(&keep),
        &runner,
        &[
            ("RUNNER_TEMP", runner.to_str().unwrap()),
            ("PATHINFO", &file("info.json")),
            ("KEPT", &file("kept.txt")),
            ("OUTPUTS", &file("outputs")),
            ("COPIED", &file("copied")),
        ],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert!(runner.join("keep").is_dir());
    assert_eq!(
        read(&runner.join("copied")).lines().collect::<Vec<_>>(),
        [
            format!(
                "store {}",
                in_store("aaa-git-2.54 bbb-vm-test-run-proj eee-kept-before")
            ),
            format!("kept {}", in_store("hhh-tmux hhh-tmux-man")),
        ]
    );
    assert!(
        ran.stdout
            .contains("kept 3 paths from this gate and 2 from the kept builds"),
        "{}",
        ran.said()
    );
}
