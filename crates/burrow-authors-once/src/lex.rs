//! Just enough of Nix to find names: comments and string contents dropped,
//! what is left as words (paths like `config.burrow.authors`, or file
//! paths) and single punctuation characters.

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    /// A name, a dotted path of names, or a file path.
    Word(String),
    Punct(char),
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '\'' | '.' | '/' | '~')
}

pub fn lex(src: &str) -> Vec<Tok> {
    let s: Vec<char> = src.chars().collect();
    let mut out: Vec<Tok> = Vec::new();
    let mut i = 0;
    let mut last_end: Option<usize> = None;
    while i < s.len() {
        let c = s[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '#' {
            while i < s.len() && s[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && s.get(i + 1) == Some(&'*') {
            i += 2;
            while i < s.len() && !(s[i] == '*' && s.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if c == '"' {
            let glued = last_end == Some(i);
            let (text, next) = string(&s, i + 1, &mut out);
            i = next;
            if is_name(&s, i, &out, glued) {
                quoted(&mut out, text, glued);
                last_end = Some(i);
            }
        } else if c == '\'' && s.get(i + 1) == Some(&'\'') {
            i = indented(&s, i + 2, &mut out);
        } else if word_char(c) {
            let start = i;
            while i < s.len() && word_char(s[i]) {
                i += 1;
            }
            let text: String = s[start..i].iter().collect();
            match out.last_mut() {
                Some(Tok::Word(w)) if last_end == Some(start) && w.ends_with('"') => {
                    w.push_str(&text)
                }
                _ => out.push(Tok::Word(text)),
            }
            last_end = Some(i);
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    out
}

/// Whether the string that just ended at `i` is part of an attribute name:
/// it continues a dotted path (`x."a"`, `"a".b`), or is what an `=` binds.
/// Any other string is data.
fn is_name(s: &[char], i: usize, out: &[Tok], glued: bool) -> bool {
    let after_dot = matches!(out.last(), Some(Tok::Word(w)) if w.ends_with('.'));
    if after_dot || glued || s.get(i) == Some(&'.') {
        return true;
    }
    let rest = s[i.min(s.len())..].iter().skip_while(|c| c.is_whitespace());
    let mut rest = rest.copied();
    rest.next() == Some('=') && rest.next() != Some('=')
}

/// A quoted attribute name (`x."authors" = `) is a name; any other string
/// is data, and dropped.
fn quoted(out: &mut Vec<Tok>, text: Option<String>, glued: bool) {
    let Some(text) = text else { return };
    match out.last_mut() {
        Some(Tok::Word(w)) if w.ends_with('.') || (glued && !w.is_empty()) => {
            w.push_str(&format!("\"{text}\""))
        }
        _ => out.push(Tok::Word(format!("\"{text}\""))),
    }
}

/// The authors' type file, named in a string (`import "${./x}/github-author.nix"`),
/// is a word, as its path is.
fn note_type(text: &str, sink: &mut Vec<Tok>) {
    if text.contains("github-author") && !text.contains(char::is_whitespace) {
        sink.push(Tok::Word("github-author".into()));
    }
}

/// The contents of a `"` string from `i`, if it has no interpolation, and
/// where it ends. The tokens of each interpolation go to `sink`, as code.
fn string(s: &[char], mut i: usize, sink: &mut Vec<Tok>) -> (Option<String>, usize) {
    let mut text = String::new();
    let mut plain = true;
    while i < s.len() && s[i] != '"' {
        if s[i] == '\\' {
            i += 1;
        } else if s[i] == '$' && s.get(i + 1) == Some(&'{') {
            plain = false;
            i = interpolation(s, i + 2, sink);
            continue;
        } else if i < s.len() {
            text.push(s[i]);
        }
        i += 1;
    }
    note_type(&text, sink);
    (plain.then_some(text), i + 1)
}

/// The end of a `''` string from `i`; the tokens of its interpolations go
/// to `sink`.
fn indented(s: &[char], mut i: usize, sink: &mut Vec<Tok>) -> usize {
    let mut text = String::new();
    while i < s.len() {
        if s[i] == '\'' && s.get(i + 1) == Some(&'\'') {
            match s.get(i + 2) {
                Some('$' | '\'') => i += 3,
                Some('\\') => i += 4,
                _ => {
                    note_type(&text, sink);
                    return i + 2;
                }
            }
        } else if s[i] == '$' && s.get(i + 1) == Some(&'{') {
            i = interpolation(s, i + 2, sink);
        } else {
            text.push(s[i]);
            i += 1;
        }
    }
    note_type(&text, sink);
    i
}

/// The end of the `${ }` from `i` (just inside it), its code lexed into
/// `sink`: an interpolation is Nix, and can bind or read authors.
fn interpolation(s: &[char], i: usize, sink: &mut Vec<Tok>) -> usize {
    let end = skip(s, i);
    let stop = end.saturating_sub(1).max(i).min(s.len());
    let inner: String = s[i.min(stop)..stop].iter().collect();
    sink.extend(lex(&inner));
    end
}

/// The end of the `${ }` from `i`: its braces balanced, strings in it
/// skipped whole.
fn skip(s: &[char], mut i: usize) -> usize {
    let mut depth = 1;
    while i < s.len() && depth > 0 {
        match s[i] {
            '{' => depth += 1,
            '}' => depth -= 1,
            '#' => {
                while i < s.len() && s[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '/' if s.get(i + 1) == Some(&'*') => {
                i += 2;
                while i < s.len() && !(s[i] == '*' && s.get(i + 1) == Some(&'/')) {
                    i += 1;
                }
                i += 2;
                continue;
            }
            '"' => {
                i = string(s, i + 1, &mut Vec::new()).1;
                continue;
            }
            '\'' if s.get(i + 1) == Some(&'\'') => {
                i = indented(s, i + 2, &mut Vec::new());
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    i
}
