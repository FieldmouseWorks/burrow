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
         in a file on the machine (the settings overlays under `burrow.stateDir`,\n\
         and the usage hold in proj's jobs directory), and its readers read it\n\
         again at their next decision.\n\
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
        out.push_str(&line(&row));
    }
    out
}

/// One table row, a pipe in a cell escaped: it would end the cell.
fn line(cells: &[String]) -> String {
    let cells: Vec<_> = cells.iter().map(|cell| cell.replace('|', "\\|")).collect();
    format!("| {} |\n", cells.join(" | "))
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
        Kind::Percent | Kind::Usd => "number".into(),
        Kind::Date => "date".into(),
        Kind::OneOf(_) => "word".into(),
        Kind::Engine => "engine".into(),
        Kind::Repo => "repository".into(),
        Kind::Maybe(kind) => format!("{} or null", kind_type(kind)),
        Kind::Omitted(kind) => format!("{}, optional", kind_type(kind)),
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
        Kind::Percent => "0 to 100".into(),
        Kind::Usd => "dollars, 0 or more, to the cent".into(),
        Kind::Date => "`YYYY-MM-DD`".into(),
        Kind::Text | Kind::Object => "any".into(),
        Kind::OneOf(words) => words.iter().map(|w| code(w)).collect::<Vec<_>>().join(", "),
        Kind::Engine => format!(
            "`[a-z0-9][a-z0-9-]*` (configured engine plugin), never {}",
            crate::roles::RETIRED_ENGINES
                .iter()
                .map(|w| code(w))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Kind::Repo => "`OWNER/NAME`".into(),
        Kind::Maybe(kind) => format!("{}, or `null`", allowed(kind)),
        Kind::Omitted(kind) => format!("{}, or left out", allowed(kind)),
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
        let path = burrow_test::manifest_dir!().join("../..").join(PATH);
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
            "| `settings.pipelineUntil` | integer or null | 0 or more, or `null` | `null` | yes | proj | `settings/overlay.json` |"
        ));
        // A pipe in a cell would end it.
        assert_eq!(
            line(&["`(^|/)x`".into(), "y".into()]),
            "| `(^\\|/)x` | y |\n"
        );
    }
}
