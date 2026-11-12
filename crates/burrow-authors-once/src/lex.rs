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
            let (text, next) = string(&s, i + 1);
            i = next;
            quoted(&mut out, text);
        } else if c == '\'' && s.get(i + 1) == Some(&'\'') {
            i = indented(&s, i + 2);
        } else if word_char(c) {
            let start = i;
            while i < s.len() && word_char(s[i]) {
                i += 1;
            }
            out.push(Tok::Word(s[start..i].iter().collect()));
        } else {
            out.push(Tok::Punct(c));
            i += 1;
        }
    }
    // A string that isn't a name (nothing binds it) is data.
    let mut kept = Vec::new();
    for (n, tok) in out.iter().enumerate() {
        let quoted = matches!(tok, Tok::Word(w) if w.starts_with('"'));
        if !quoted || out.get(n + 1) == Some(&Tok::Punct('=')) {
            kept.push(tok.clone());
        }
    }
    kept
}

/// A quoted attribute name (`x."authors" = `) is a name; any other string
/// is data, and dropped.
fn quoted(out: &mut Vec<Tok>, text: Option<String>) {
    let Some(text) = text else { return };
    match out.last_mut() {
        Some(Tok::Word(w)) if w.ends_with('.') => w.push_str(&format!("\"{text}\"")),
        _ => out.push(Tok::Word(format!("\"{text}\""))),
    }
}

/// The contents of a `"` string from `i`, if it has no interpolation, and
/// where it ends.
fn string(s: &[char], mut i: usize) -> (Option<String>, usize) {
    let mut text = String::new();
    let mut plain = true;
    while i < s.len() && s[i] != '"' {
        if s[i] == '\\' {
            i += 1;
        } else if s[i] == '$' && s.get(i + 1) == Some(&'{') {
            plain = false;
            i = interpolation(s, i + 2);
            continue;
        } else if i < s.len() {
            text.push(s[i]);
        }
        i += 1;
    }
    (plain.then_some(text), i + 1)
}

/// The end of a `''` string from `i`.
fn indented(s: &[char], mut i: usize) -> usize {
    while i < s.len() {
        if s[i] == '\'' && s.get(i + 1) == Some(&'\'') {
            match s.get(i + 2) {
                Some('$' | '\'') => i += 3,
                Some('\\') => i += 4,
                _ => return i + 2,
            }
        } else if s[i] == '$' && s.get(i + 1) == Some(&'{') {
            i = interpolation(s, i + 2);
        } else {
            i += 1;
        }
    }
    i
}

/// The end of the `${ }` from `i` (just inside it): its braces balanced,
/// strings in it skipped whole.
fn interpolation(s: &[char], mut i: usize) -> usize {
    let mut depth = 1;
    while i < s.len() && depth > 0 {
        match s[i] {
            '{' => depth += 1,
            '}' => depth -= 1,
            '"' => {
                i = string(s, i + 1).1;
                continue;
            }
            '\'' if s.get(i + 1) == Some(&'\'') => {
                i = indented(s, i + 2);
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    i
}
