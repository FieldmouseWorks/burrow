use super::*;
use burrow_settings::reference::PATH;
use burrow_settings::schema::SETTINGS;

fn found(text: &str) -> Vec<String> {
    check_text("docs/x.md", text, &facts())
        .into_iter()
        .map(|f| format!("{}: {}", f.line, f.what))
        .collect()
}

/// Lines main's README and front door had before they linked the
/// reference: each states a value or a file the reference owns.
#[test]
fn what_the_guidance_said_before_fails() {
    assert_eq!(
        found(
            "tells you once. `burrow.proj.usage.paid` defaults to false:\npaid extra usage is off."
        ),
        ["1: `proj.usage.paid` with a value"]
    );
    assert_eq!(
        found("After a reset, `burrow.proj.usageTrickle` (default 2) caps concurrent Claude"),
        ["1: `proj.usageTrickle` with a value"]
    );
    assert_eq!(
        found(
            "Codex jobs have their own small\npool (`burrow.proj.codex.slots`, two by default), leaving"
        ),
        ["1: `proj.codex.slots` with a value"]
    );
    assert_eq!(
        found("prove a mismatch fails. Codex uses its own slot pool (`codex.slots`, two)"),
        ["1: `proj.codex.slots` with a value"]
    );
    assert_eq!(
        found("Roles on the box live in `~/.config/proj/roles`: `ROLE ENGINE`, with"),
        ["1: where `proj.roles` is kept (`~/.config/proj/roles`)"]
    );
    assert_eq!(
        found("- Saved engine setups live beside the roles file in `roles.d/NAME`:"),
        ["1: where `proj.engineSetups` is kept (`roles.d/`)"]
    );
    assert_eq!(
        found("| `proj.usageHold` | proj's usage record, `.usage.json` in its jobs dir |"),
        ["1: where `proj.usageHold` is kept (`.usage.json`)"]
    );
}

#[test]
fn naming_an_option_without_its_value_passes() {
    for text in [
        // Named, no value.
        "At most `maxJobs` Claude turns run at\nonce, `maxJobsPerProject` from one project, and the rest queue.",
        "With `triage.drain` set to an engine, the drain hands `ready` issues to it.",
        // A number too far from the name to be its value.
        "open PR's wait as its status says it (`stallAfterMinutes`, longer for a\nhold), due at once when one of its steps has failed three times in a row",
        // Not a declared option: proj's own wait, or a word every option's
        // tail could be.
        "every job's turn waiting in line (`proj.stallAfterMinutes`, 30 by default)",
        "the review `rounds` (5 by default)",
        // A path that only starts like a live file's.
        "the `~/.config/proj/roles-old` file",
        // A value in a sentence of its own.
        "Set `proj.maxJobs` in the flake. Two of them are Codex's.",
    ] {
        assert_eq!(found(text), Vec::<String>::new(), "{text}");
    }
}

#[test]
fn the_owner_may_state_its_facts() {
    let text =
        "| `proj.maxJobs` | integer | 1 or more | `4` |\n| `proj.roles` | `~/.config/proj/roles` |";
    assert!(check_text(PATH, text, &facts()).is_empty());
    assert_eq!(check_text("README.md", text, &facts()).len(), 2);
}

#[test]
fn every_option_is_known_by_its_names() {
    let facts = facts();
    for s in SETTINGS {
        for name in [s.name.to_owned(), format!("burrow.{}", s.name)] {
            let text = format!("`{name}` defaults to 9.");
            let hits = check_text("a.md", &text, &facts);
            assert!(
                hits.iter()
                    .any(|f| f.what == format!("`{}` with a value", s.name)),
                "{text}: {hits:?}"
            );
        }
    }
}

fn aliases(name: &str) -> Vec<String> {
    let parts: Vec<_> = name.split('.').collect();
    let mut names: Vec<_> = (0..parts.len() - 1).map(|i| parts[i..].join(".")).collect();
    let last = parts.last().unwrap();
    if last.chars().any(|c| c.is_ascii_uppercase()) {
        names.push((*last).to_owned());
    }
    let prefixed: Vec<_> = names.iter().map(|n| format!("burrow.{n}")).collect();
    names.extend(prefixed);
    names
}

#[test]
fn numeric_values_are_found_in_markdown_for_every_alias() {
    let facts = facts();
    for setting in SETTINGS {
        for name in aliases(setting.name) {
            for text in [
                format!("`{name}` is 4."),
                format!("`{name}` is `4`."),
                format!("| `{name}` | `4` |"),
                format!("| {name} | 4 |"),
                format!("`{name}=4`"),
                format!("{name}=4"),
                format!("`{name} = 4;`"),
            ] {
                let hits = check_text("a.md", &text, &facts);
                assert!(
                    hits.iter()
                        .any(|f| f.what == format!("`{}` with a value", setting.name)),
                    "{text}: {hits:?}"
                );
            }
            for text in [
                format!(
                    "`{name}` is described elsewhere with enough intervening words before `4`."
                ),
                format!("`{name}` is documented. `4` workers are available."),
                format!("`{name}` is documented; 4 workers are available."),
                format!("`{name}` is documented? 4 workers are available."),
                format!("`{name}` is documented! 4 workers are available."),
                format!("`{name}` is used by worker4 and 4workers."),
            ] {
                assert!(check_text("a.md", &text, &facts).is_empty(), "{text}");
            }
        }
    }
}

#[test]
fn colon_pairs_keep_values_with_every_alias_and_wrapped_list_item() {
    let facts = facts();
    for setting in SETTINGS {
        for name in aliases(setting.name) {
            for value in ["4", "`4`", "false", "`false`"] {
                for text in [
                    format!("`{name}`: {value}"),
                    format!("The option `{name}`: {value}."),
                    format!("- `{name}`: {value}"),
                    format!("- `{name}`:\n  {value}"),
                    format!("1. `{name}`:\n   {value}"),
                    format!("The option `{name}`:\n{value}."),
                ] {
                    let hits = check_text("a.md", &text, &facts);
                    assert!(
                        hits.iter()
                            .any(|f| f.what == format!("`{}` with a value", setting.name)),
                        "{text}: {hits:?}"
                    );
                }
            }
        }
    }
    assert_eq!(
        found("- `proj.maxJobs`:\n  `4`\n- `proj.usage.paid`:\n  false"),
        [
            "1: `proj.maxJobs` with a value",
            "3: `proj.usage.paid` with a value",
        ]
    );
    assert!(found("`proj.maxJobs`:\n\n4 workers are available.").is_empty());
    assert!(found("- `proj.maxJobs`:\n- 4 workers are available.").is_empty());
}

#[test]
fn defaults_need_an_actual_value() {
    let facts = facts();
    for setting in SETTINGS {
        for name in aliases(setting.name) {
            for text in [
                format!("See the [reference](settings-reference.md) for `{name}` defaults."),
                format!("See `{name}` defaults."),
                format!("See the default for `{name}`."),
                format!("See the [reference](settings-reference.md) for the default of `{name}`."),
            ] {
                assert!(check_text("a.md", &text, &facts).is_empty(), "{text}");
            }
            if let Some(value) = setting.default_value() {
                let text = format!("`{name}` defaults to `{value}`.");
                let hits = check_text("a.md", &text, &facts);
                assert!(
                    hits.iter()
                        .any(|f| f.what == format!("`{}` with a value", setting.name)),
                    "{text}: {hits:?}"
                );
            }
        }
    }
    for text in [
        "`proj.maxJobs` defaults to `4`.",
        "The default for `proj.maxJobs` is 4.",
        "`proj.usage.paid` defaults to false.",
        "The default for `proj.usage.paid` is `false`.",
        "`proj.roles` defaults to null.",
        "`triage.drain` defaults to claude.",
    ] {
        assert!(!found(text).is_empty(), "{text}");
    }
}

#[test]
fn sentences_keep_their_lines() {
    let text = "# Title `proj.maxJobs`\n\nOne line. Then\nanother: and\n\n- an item\n  wrapped\n\n```\ncode 1\n```\n| a | b |\n";
    let got: Vec<_> = sentences(text);
    assert_eq!(
        got,
        [
            (1, "# Title `proj.maxJobs`".to_owned()),
            (3, "One line.".to_owned()),
            (3, "Then another: and".to_owned()),
            (6, "- an item wrapped".to_owned()),
            (10, "code 1".to_owned()),
            (12, "| a | b |".to_owned()),
        ]
    );
}

#[test]
fn a_tree_is_every_markdown_file_but_build_output() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    for (path, text) in [
        ("README.md", "pool (`codex.slots`, two)"),
        ("docs/a.md", "`usageTrickle` (default 2)"),
        ("docs/b.txt", "`usageTrickle` (default 2)"),
        (".git/c.md", "`usageTrickle` (default 2)"),
        ("target/d.md", "`usageTrickle` (default 2)"),
        (PATH, "`usageTrickle` (default 2)"),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    let got: Vec<_> = check(root, &facts())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(
        got,
        [
            "README.md:1: states `proj.codex.slots` with a value; link docs/settings-reference.md instead",
            "docs/a.md:1: states `proj.usageTrickle` with a value; link docs/settings-reference.md instead",
        ]
    );
}
