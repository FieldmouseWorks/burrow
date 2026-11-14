//! gh, read as gh reads its command line, for what codex.rs and first.rs
//! refuse: its command, the words after it, its options as pflag (gh's
//! option parser) reads them, and `gh api`'s method, endpoints and body.
//! Each of them decides what counts; this only reads.

use crate::shell;

/// Whether a word holds an expansion or a substitution, which the guard
/// can't read to its value (and which, unquoted, may split into several
/// words, options among them). The plain shell reading keeps no quotes, so
/// a literal `$` reads as one too, as in curl.rs; an unquoted glob is
/// marked as one, so a quoted `?` reads as plain text.
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

/// A gh command's options, for pflag to read its words by: the long ones
/// that take a value (any other is a switch, `--name[=true|false]`), and
/// each short letter with its long name and whether it takes one.
pub(crate) struct Spec {
    pub values: &'static [&'static str],
    pub shorts: &'static [(char, &'static str, bool)],
}

/// One option, as pflag reads it: its long name (a short letter gh doesn't
/// know reads as itself), its value, if it takes one or is given one with
/// `=`.
pub(crate) struct Opt<'a> {
    pub name: &'a str,
    pub value: Option<&'a str>,
}

/// A command's words, read by its [`Spec`]: options, and the rest
/// (positional words).
pub(crate) struct Args<'a> {
    pub opts: Vec<Opt<'a>>,
    pub positional: Vec<&'a str>,
}

/// `words` as pflag reads them by `spec`. `--name=value` or `--name value`
/// for a long option that takes a value (the next word, whatever it looks
/// like); short ones run together (`-wl first`, `-wlfirst`), a switch's
/// letter going on to the next and one that takes a value taking the rest
/// of the word, after a `=` if it starts with one, or else the next word.
/// `--` ends the options, and `-` alone is a positional word.
pub(crate) fn args<'a>(words: &[&'a str], spec: &Spec) -> Args<'a> {
    let mut args = Args {
        opts: Vec::new(),
        positional: Vec::new(),
    };
    let mut at = 0;
    let next = |at: &mut usize| {
        *at += 1;
        words.get(*at).copied()
    };
    while let Some(&w) = words.get(at) {
        if w == "--" {
            args.positional.extend(words.iter().copied().skip(at + 1));
            break;
        }
        if let Some(long) = w.strip_prefix("--") {
            let (name, joined) = long
                .split_once('=')
                .map_or((long, None), |(n, v)| (n, Some(v)));
            let value = if joined.is_none() && spec.values.contains(&name) {
                next(&mut at)
            } else {
                joined
            };
            args.opts.push(Opt { name, value });
        } else if let Some(short) = w.strip_prefix('-').filter(|s| !s.is_empty()) {
            let mut rest = short;
            while let Some(c) = rest.chars().next() {
                let after = &rest[c.len_utf8()..];
                let (name, takes) = spec
                    .shorts
                    .iter()
                    .find(|(s, ..)| *s == c)
                    .map_or((&rest[..c.len_utf8()], false), |(_, n, t)| (*n, *t));
                if let Some(v) = after.strip_prefix('=') {
                    args.opts.push(Opt {
                        name,
                        value: Some(v),
                    });
                    break;
                }
                if !takes {
                    args.opts.push(Opt { name, value: None });
                    rest = after;
                    continue;
                }
                let value = if after.is_empty() {
                    next(&mut at)
                } else {
                    Some(after)
                };
                args.opts.push(Opt { name, value });
                break;
            }
        } else {
            args.positional.push(w);
        }
        at += 1;
    }
    args
}

/// `gh api`'s options.
pub(crate) const API: Spec = Spec {
    values: &[
        "method",
        "field",
        "raw-field",
        "header",
        "hostname",
        "input",
        "jq",
        "template",
        "preview",
        "cache",
    ],
    shorts: &[
        ('X', "method", true),
        ('f', "raw-field", true),
        ('F', "field", true),
        ('H', "header", true),
        ('q', "jq", true),
        ('t', "template", true),
        ('p', "preview", true),
        ('i', "include", false),
    ],
};

/// A `-f` (raw) or `-F` (typed) field of `gh api`: `key=value`.
pub(crate) struct Field<'a> {
    pub text: &'a str,
    /// `-F`, whose `@file` value is read from the file, and whose
    /// `{owner}`, `{repo}` and `{branch}` gh fills in.
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
        let read = args(words, &API);
        let mut api = Api {
            method: None,
            fields: Vec::new(),
            input: false,
            urls: read.positional.clone(),
            unreadable: words.iter().copied().any(expands),
        };
        for opt in read.opts {
            match opt.name {
                "method" => api.method = opt.value,
                "field" | "raw-field" => {
                    if let Some(text) = opt.value {
                        api.fields.push(Field {
                            text,
                            typed: opt.name == "field",
                        });
                    }
                }
                "input" => api.input = true,
                _ => {}
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
