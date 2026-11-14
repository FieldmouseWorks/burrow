//! The settings' facts, from burrow-settings' schema: an option's value and
//! the file a live one is kept in are stated only in the generated reference.

use burrow_settings::reference::PATH;
use burrow_settings::schema::{Change, Kind, SETTINGS, Setting};
use regex::{Regex, escape};
use serde_json::Value;

use crate::{Fact, Near};

/// How close to its name a value has to be to be the option's: a few
/// words, as in "(default 2)" or "defaults to false".
const NEAR: usize = 24;

/// Words that state a value: a number standing alone, a number word, a
/// JSON constant, or an allowed word. `one` is left out: "one
/// project" is how the guidance counts things, not a setting's value.
/// Word boundaries include Markdown delimiters and compact assignments,
/// while excluding digits embedded in unrelated words such as `worker4`.
const VALUE: &str =
    r"(?i)\b(?:\d+|two|three|four|five|six|seven|eight|nine|ten|twelve|true|false|null)\b";

/// Every option's facts: named with a value, and a live one's file.
pub fn facts() -> Vec<Fact> {
    SETTINGS
        .iter()
        .flat_map(|s| {
            let valued = Fact {
                what: format!("`{}` with a value", s.name),
                owner: PATH,
                names: names(s),
                with: Some(Near {
                    pattern: values(s),
                    chars: NEAR,
                }),
            };
            let file = match s.change {
                Change::Live { file } => Some(Fact {
                    what: format!("where `{}` is kept (`{file}`)", s.name),
                    owner: PATH,
                    names: path(file),
                    with: None,
                }),
                Change::Deploy => None,
            };
            [Some(valued), file].into_iter().flatten()
        })
        .collect()
}

/// A file's path, whole: `roles` isn't `roles-old`, but a directory's
/// path (`roles.d/`) takes any name in it. No live option is kept in a
/// directory today; the tests below hold the rule for the next one.
fn path(file: &str) -> Regex {
    let end = if file.ends_with('/') {
        ""
    } else {
        r"(?:$|[^\w-])"
    };
    Regex::new(&format!("{}{end}", escape(file))).expect("an escaped path is a regex")
}

/// How the guidance names the option: its name or a dotted tail of it of
/// two parts or more, `burrow.` before it or not, or its last part alone
/// when that's a word of its own making (`maxJobs`, not `rounds`).
fn names(s: &Setting) -> Regex {
    let parts: Vec<_> = s.name.split('.').collect();
    let mut tails: Vec<String> = (0..parts.len().saturating_sub(1))
        .map(|i| escape(&parts[i..].join(".")))
        .collect();
    let last = parts[parts.len() - 1];
    if last.chars().any(|c| c.is_ascii_uppercase()) {
        tails.push(escape(last));
    }
    Regex::new(&format!(
        r"(?:^|[^\w.])(?:burrow\.)?(?:{})(?:$|[^\w.]|\.(?:\s|$))",
        tails.join("|")
    ))
    .expect("an option's names are a regex")
}

/// What states its value: any of [`VALUE`], a word its kind allows, or
/// its schema's JSON collection or text default (which may contain no
/// value words).
fn values(s: &Setting) -> Regex {
    let words: Vec<_> = words(s.kind).iter().map(|w| escape(w)).collect();
    let own = if words.is_empty() {
        String::new()
    } else {
        format!(r"|\b(?:{})\b", words.join("|"))
    };
    let collection = s
        .default_value()
        .filter(|value| value.is_array() || value.is_object() || value.is_string())
        .map(|value| format!(r"|(?:^|[^\]]){}", collection(&value)))
        .unwrap_or_default();
    Regex::new(&format!("{VALUE}{own}{collection}")).expect("values are a regex")
}

/// A JSON collection as Markdown may spell it: any whitespace between its
/// tokens (`[ ]`, `{ }`, one item a line), and any punctuation escaped
/// with a backslash, as a table cell escapes `|`. Not right after a `]`:
/// `[name][]` is a collapsed reference link, not an empty list.
fn collection(value: &Value) -> String {
    let join = |items: Vec<String>| items.join(&format!(r"\s*{}\s*", markdown(",")));
    match value {
        Value::Array(items) => format!(
            r"{}\s*{}\s*{}",
            markdown("["),
            join(items.iter().map(collection).collect()),
            markdown("]")
        ),
        Value::Object(map) => format!(
            r"{}\s*{}\s*{}",
            markdown("{"),
            join(
                map.iter()
                    .map(|(k, v)| {
                        format!(
                            r"{}\s*{}\s*{}",
                            markdown(&Value::String(k.clone()).to_string()),
                            markdown(":"),
                            collection(v)
                        )
                    })
                    .collect()
            ),
            markdown("}")
        ),
        scalar => markdown(&scalar.to_string()),
    }
}

/// `text` literally, each ASCII punctuation mark in it with or without the
/// backslash that escapes it in Markdown.
fn markdown(text: &str) -> String {
    text.chars()
        .map(|c| {
            let literal = escape(&c.to_string());
            if c.is_ascii_punctuation() {
                format!(r"\\?{literal}")
            } else {
                literal
            }
        })
        .collect()
}

fn words(kind: &Kind) -> &'static [&'static str] {
    match kind {
        Kind::OneOf(words) => words,
        Kind::Maybe(kind) | Kind::List(kind) => words(kind),
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::path;

    #[test]
    fn a_file_is_whole_and_a_directory_takes_any_name_in_it() {
        let file = path("roles");
        assert!(file.is_match("in `roles`: one line"));
        assert!(!file.is_match("the `roles-old` file"));
        let dir = path("roles.d/");
        assert!(dir.is_match("beside it in `roles.d/NAME`:"));
        assert!(!dir.is_match("the `roles.d` link"));
    }
}
