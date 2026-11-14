//! gh, read as gh reads its command line, for what codex.rs and first.rs
//! refuse: its command, the words after it, and `gh api`'s method,
//! endpoints and body. Each of them decides what counts; this only reads.

use crate::shell;

/// Whether a word holds an expansion or a substitution, which the guard
/// can't read to its value (and which, unquoted, may split into several
/// words, options among them). The shell reading keeps no quotes, so a
/// literal `$` reads as one too, as in curl.rs; an unquoted glob is marked
/// as one, so a quoted `?` reads as plain text.
pub(crate) fn expands(word: &str) -> bool {
    word.contains(['$', shell::UNKNOWN])
}

/// A gh command line: its command (two words, or `api` alone) and the words
/// after it. Its own options may come before its command, `-R` with a value.
pub(crate) struct Gh<'a> {
    pub command: Vec<&'a str>,
    pub rest: Vec<&'a str>,
}

/// What gh does with these words, after `gh`.
pub(crate) fn read<'a>(words: impl Iterator<Item = &'a String>) -> Gh<'a> {
    let mut words = words.map(String::as_str);
    let mut command = Vec::new();
    while command.len() < 2 && command.first() != Some(&"api") {
        let Some(w) = words.next() else { break };
        match w {
            "-R" | "--repo" => {
                words.next();
            }
            o if o.starts_with('-') => {}
            o => command.push(o),
        }
    }
    Gh {
        command,
        rest: words.collect(),
    }
}

/// A `-f` (raw) or `-F` (typed) field of `gh api`: `key=value`.
pub(crate) struct Field<'a> {
    pub text: &'a str,
    /// `-F`, whose `@file` value is read from the file.
    pub typed: bool,
}

/// `gh api`'s words, read as gh reads them. A brace list comes expanded
/// into the words it makes, and may make more than one endpoint, so each
/// is kept as one (gh takes one, and stops on more).
pub(crate) struct Api<'a> {
    pub method: Option<&'a str>,
    pub fields: Vec<Field<'a>>,
    /// `--input`: a body from a file or stdin.
    pub input: bool,
    pub urls: Vec<&'a str>,
    /// Some word holds an expansion (or an unquoted glob), which, unquoted,
    /// may split into words that change which one is the endpoint, or
    /// carry the method or the query.
    pub unreadable: bool,
}

impl<'a> Api<'a> {
    pub(crate) fn read(words: &[&'a str]) -> Api<'a> {
        let mut api = Api {
            method: None,
            fields: Vec::new(),
            input: false,
            urls: Vec::new(),
            unreadable: words.iter().copied().any(expands),
        };
        let mut words = words.iter().copied();
        while let Some(w) = words.next() {
            let (name, joined) = match w.split_once('=') {
                Some((n, v)) if n.starts_with("--") => (n, Some(v)),
                _ => match w.get(..2) {
                    Some(n @ ("-X" | "-f" | "-F" | "-H" | "-q" | "-t" | "-p")) if w.len() > 2 => {
                        (n, Some(&w[2..]))
                    }
                    _ => (w, None),
                },
            };
            let mut value = || joined.or_else(|| words.next());
            match name {
                "-X" | "--method" => api.method = value(),
                "-f" | "--raw-field" | "-F" | "--field" => {
                    let typed = matches!(name, "-F" | "--field");
                    if let Some(text) = value() {
                        api.fields.push(Field { text, typed });
                    }
                }
                "--input" => {
                    api.input = true;
                    value();
                }
                "-H" | "--header" | "-q" | "--jq" | "-t" | "--template" | "--hostname" | "-p"
                | "--preview" | "--cache" => {
                    value();
                }
                o if o.starts_with('-') => {}
                _ => api.urls.push(w),
            }
        }
        api
    }

    /// Whether it writes: an explicit method other than GET, or fields or
    /// input, which make it a POST.
    pub(crate) fn writes(&self) -> bool {
        match self.method {
            Some(m) => !(m.eq_ignore_ascii_case("GET") || m.eq_ignore_ascii_case("HEAD")),
            None => !self.fields.is_empty() || self.input,
        }
    }

    /// Whether its GraphQL query comes from where the guard can't read it:
    /// a file or input.
    pub(crate) fn unread_query(&self) -> bool {
        self.input
            || self
                .fields
                .iter()
                .any(|f| f.typed && f.text.starts_with("query=@"))
    }
}
