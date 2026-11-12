//! The settings' facts, from burrow-settings' schema: an option's value and
//! the file a live one is kept in are stated only in the generated reference.

use burrow_settings::reference::PATH;
use burrow_settings::schema::{Change, Kind, SETTINGS, Setting};
use regex::{Regex, escape};

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
/// path (`roles.d/`) takes any name in it.
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
/// its schema's JSON collection default (which may contain no value words).
fn values(s: &Setting) -> Regex {
    let words: Vec<_> = words(s.kind).iter().map(|w| escape(w)).collect();
    let own = if words.is_empty() {
        String::new()
    } else {
        format!(r"|\b(?:{})\b", words.join("|"))
    };
    let collection = s
        .default_value()
        .filter(|value| value.is_array() || value.is_object())
        .map(|value| format!("|{}", escape(&value.to_string())))
        .unwrap_or_default();
    Regex::new(&format!("{VALUE}{own}{collection}")).expect("values are a regex")
}

fn words(kind: &Kind) -> &'static [&'static str] {
    match kind {
        Kind::OneOf(words) => words,
        Kind::Maybe(kind) | Kind::List(kind) => words(kind),
        _ => &[],
    }
}
