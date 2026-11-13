//! docs/settings-reference.md, written from the schema: every option's
//! name, type, allowed values, default, whether it changes live, the part
//! that reads it, where its value is kept, and what it is. The guidance
//! links it rather than restating any of that; `burrow-settings reference`
//! prints it, and a test fails when the file isn't what it prints.

use crate::schema::{Change, Fallback, Kind, SETTINGS};

/// Where the reference is kept, from the repo's root.
pub const PATH: &str = "docs/settings-reference.md";

/// The reference, as Markdown.
pub fn markdown() -> String {
    let mut out = String::from(
        "# Settings reference\n\
         \n\
         <!-- Written by `burrow-settings reference` from\n\
         crates/burrow-settings/src/schema.rs: change the schema, not this file. -->\n\
         \n\
         Every pipeline option, declared once in burrow-settings' schema\n\
         ([crates/burrow-settings/src/schema.rs](../crates/burrow-settings/src/schema.rs)).\n\
         This is the one place the guidance gives an option's value or where it's\n\
         kept: anything else links here. `proj settings show` prints the value in force\n\
         on the machine and where it came from; [settings.md](settings.md) says how\n\
         they're read and changed.\n\
         \n\
         An option that isn't live is set in the machine's flake, under `burrow.`\n\
         and its name: a change is a PR to the flake and a deploy. A live one is kept\n\
         in a file on the machine (the settings overlay under `burrow.stateDir`;\n\
         proj's in the job user's `~/.config/proj`, setups in `roles.d/` beside the\n\
         roles file, and the usage hold in proj's jobs directory), and its readers\n\
         read it again at their next decision.\n\
         \n\
         | Option | Type | Allowed | Default | Live | Owner | Kept in | What it is |\n\
         |---|---|---|---|---|---|---|---|\n",
    );
    for s in SETTINGS {
        let default = match s.default {
            Fallback::Value(_) => code(&s.default_value().expect("a value").to_string()),
            Fallback::Derived { says, .. } => says.to_owned(),
        };
        let (live, kept) = match s.change {
            Change::Deploy => (
                "no",
                format!("{} in the flake", code(&format!("burrow.{}", s.name))),
            ),
            Change::Live { file } => ("yes", code(file)),
        };
        let row = [
            code(s.name),
            kind_type(s.kind),
            allowed(s.kind),
            default,
            live.to_owned(),
            s.owner.to_owned(),
            kept,
            s.help.to_owned(),
        ];
        let row: Vec<_> = row.iter().map(|cell| cell.replace('|', "\\|")).collect();
        out.push_str(&format!("| {} |\n", row.join(" | ")));
    }
    out
}

fn code(text: &str) -> String {
    format!("`{text}`")
}

/// The shape of a value.
fn kind_type(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "boolean".into(),
        Kind::Count { .. } => "integer".into(),
        Kind::Text => "text".into(),
        Kind::OneOf(_) => "word".into(),
        Kind::Repo => "repository".into(),
        Kind::Maybe(kind) => format!("{} or null", kind_type(kind)),
        Kind::List(kind) => format!("list of {}", kind_type(kind)),
        Kind::Map(kind) => format!("map of {}", kind_type(kind)),
        Kind::Record(fields) => format!(
            "{{{}}}",
            fields
                .iter()
                .map(|f| format!("{}: {}", f.name, kind_type(f.kind)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Kind::Object => "object".into(),
    }
}

/// What a value of the shape may be.
fn allowed(kind: &Kind) -> String {
    match kind {
        Kind::Bool => "`true`, `false`".into(),
        Kind::Count { min } => format!("{min} or more"),
        Kind::Text | Kind::Object => "any".into(),
        Kind::OneOf(words) => words.iter().map(|w| code(w)).collect::<Vec<_>>().join(", "),
        Kind::Repo => "`OWNER/NAME`".into(),
        Kind::Maybe(kind) => format!("{}, or `null`", allowed(kind)),
        Kind::List(kind) | Kind::Map(kind) => match allowed(kind) {
            any if any == "any" => any,
            each => format!("each {each}"),
        },
        Kind::Record(fields) => fields
            .iter()
            .map(|f| format!("{}: {}", f.name, allowed(f.kind)))
            .collect::<Vec<_>>()
            .join("; "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The file in the repo is what the schema writes. When it isn't, write
    /// it again: `cargo run -p burrow-settings -- reference > docs/settings-reference.md`.
    #[test]
    fn the_reference_is_the_schemas() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(PATH);
        let kept = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            kept == markdown(),
            "{PATH} is stale: run `cargo run -p burrow-settings -- reference > {PATH}`"
        );
    }

    #[test]
    fn every_option_has_one_row() {
        let text = markdown();
        for s in SETTINGS {
            let row = format!("| `{}` |", s.name);
            assert_eq!(text.matches(&row).count(), 1, "{}", s.name);
        }
        assert!(text.contains(
            "| `proj.roles` | map of text | any | `{}` | yes | proj | `~/.config/proj/roles` |"
        ));
        // A pipe in a cell would end it.
        assert!(text.contains(r#"`["(^\|/)secrets?/","#), "pipes escaped");
    }
}
