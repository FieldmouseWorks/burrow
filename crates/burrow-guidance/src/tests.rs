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
    // One file keeps four options: naming it states where each is kept.
    assert_eq!(
        found("Role settings live in `settings/overlay.roles.json`, beside the overlay"),
        [
            "1: where `proj.roles` is kept (`settings/overlay.roles.json`)",
            "1: where `proj.models` is kept (`settings/overlay.roles.json`)",
            "1: where `proj.engineOffers` is kept (`settings/overlay.roles.json`)",
            "1: where `proj.engineSetups` is kept (`settings/overlay.roles.json`)",
        ]
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
        "the `settings/overlay.roles.json-old` file",
        // A value in a sentence of its own.
        "Set `proj.maxJobs` in the flake. Two of them are Codex's.",
    ] {
        assert_eq!(found(text), Vec::<String>::new(), "{text}");
    }
}

#[test]
fn the_owner_may_state_its_facts() {
    let text =
        "| `proj.maxJobs` | integer | 1 or more | `4` |\n| `proj.usageHold` | `.usage.json` |";
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

/// A tree with house/ holds its AGENTS.md to each block as house/ has it:
/// what the settings integration test checked before, here so the docs
/// lane (which builds this check) runs it on an AGENTS.md-only change.
#[test]
fn the_house_blocks_copy_is_house_s() {
    let block = "<!-- house:rules v1 begin -->\nread them with `proj settings show`\n<!-- house:rules end -->\n";
    let got = |agents: Option<&str>, house: bool| -> Vec<String> {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        if house {
            fs::create_dir_all(root.join("house")).unwrap();
            fs::write(root.join("house/rules.md"), block).unwrap();
        }
        if let Some(agents) = agents {
            fs::write(root.join("AGENTS.md"), agents).unwrap();
        }
        check(root, &[])
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect()
    };
    let current = format!("# repo\n\n{block}\nIts own part.\n");
    assert!(got(Some(&current), true).is_empty());
    assert_eq!(
        got(
            Some(&current.replace("proj settings show", "settings show")),
            true
        ),
        [
            "AGENTS.md:3: carries the house block `rules` changed; copy it from house/rules.md verbatim"
        ]
    );
    for agents in [Some("# repo\n"), None] {
        assert_eq!(
            got(agents, true),
            [
                "AGENTS.md:1: carries the house block `rules` nowhere; copy it from house/rules.md verbatim"
            ]
        );
    }
    // A repo that doesn't own the blocks: its copy is house-sync's.
    assert!(got(Some("# repo\n"), false).is_empty());
}

/// `value` as JSON with a space between each of its tokens: `[ ]`, `{ }`,
/// `{ "a" : 1 }`, as Nix and hand-written Markdown spell them.
fn spaced(value: &serde_json::Value) -> String {
    use serde_json::Value;
    match value {
        Value::Array(items) if items.is_empty() => "[ ]".to_owned(),
        Value::Object(map) if map.is_empty() => "{ }".to_owned(),
        Value::Array(items) => format!(
            "[ {} ]",
            items.iter().map(spaced).collect::<Vec<_>>().join(" , ")
        ),
        Value::Object(map) => format!(
            "{{ {} }}",
            map.iter()
                .map(|(k, v)| format!("{} : {}", Value::String(k.clone()), spaced(v)))
                .collect::<Vec<_>>()
                .join(" , ")
        ),
        scalar => scalar.to_string(),
    }
}

/// Every collection default, as JSON whitespace and Markdown escaping may
/// spell it, is that option's value for every name it goes by; so is
/// skipPaths' row copied out of the reference, its pipes escaped.
#[test]
fn collection_defaults_are_found_however_spelled() {
    let facts = facts();
    let reference = burrow_settings::reference::markdown();
    let mut collections = 0;
    for setting in SETTINGS {
        let Some(value) = setting
            .default_value()
            .filter(|v| v.is_array() || v.is_object())
        else {
            continue;
        };
        collections += 1;
        let spellings = [
            value.to_string(),
            spaced(&value),
            serde_json::to_string_pretty(&value).unwrap(),
        ];
        for name in aliases(setting.name) {
            let mut texts = Vec::new();
            for json in &spellings {
                let cell = json.replace('|', "\\|").replace('\n', " ");
                texts.extend([
                    format!("`{name}` defaults to `{json}`."),
                    format!("`{name}` defaults to\n{json}"),
                    format!("| `{name}` | `{cell}` |"),
                    format!("| {name} | {cell} |"),
                    format!("`{name}` defaults to {}.", markdown_escaped(json)),
                ]);
            }
            for text in texts {
                let hits = check_text("a.md", &text, &facts);
                assert!(
                    hits.iter()
                        .any(|f| f.what == format!("`{}` with a value", setting.name)),
                    "{text}: {hits:?}"
                );
            }
        }
    }
    // settings.allowed, settings.repositoryOverrides, autoland.repos,
    // autoland.reviewers.skipPaths, proj.roles, proj.models,
    // proj.engineSetups at least.
    assert!(collections >= 7, "{collections}");
    // The reference's own row, its pipes escaped, copied out of it.
    let row = reference
        .lines()
        .find(|l| l.starts_with("| `autoland.reviewers.skipPaths` |"))
        .expect("skipPaths has a reference row");
    assert!(row.contains("\\|"), "{row}");
    for name in aliases("autoland.reviewers.skipPaths") {
        let text = row.replacen("autoland.reviewers.skipPaths", &name, 1);
        assert!(
            check_text("a.md", &text, &facts)
                .iter()
                .any(|f| f.what == "`autoland.reviewers.skipPaths` with a value"),
            "{text}"
        );
    }
}

/// `text` with every ASCII punctuation mark backslash-escaped, as Markdown
/// prose may write a literal.
fn markdown_escaped(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if c.is_ascii_punctuation() {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

/// A link to an option's reference names no value, whatever kind of link
/// it is: inline, full reference or collapsed reference (`[name][]`,
/// whose `[]` isn't an empty list), its name in code or not.
#[test]
fn links_to_an_option_state_no_value() {
    let facts = facts();
    for setting in SETTINGS {
        for name in aliases(setting.name) {
            for label in [name.clone(), format!("`{name}`")] {
                for link in [
                    format!("[{label}](settings-reference.md)"),
                    format!("[{label}](settings-reference.md#settings)"),
                    format!("[{label}][reference]"),
                    format!("[{label}][]"),
                    format!("[{label}]"),
                ] {
                    for text in [
                        format!("See the default for {link}."),
                        format!("{link} has a default."),
                        format!("- {link}: see its default"),
                    ] {
                        assert!(check_text("a.md", &text, &facts).is_empty(), "{text}");
                    }
                }
            }
        }
    }
}
