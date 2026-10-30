//! Globs as the handlers' options take them (`branches`, `issueTitles`): `*`
//! any run of characters, `/` included, `?` any one, `[...]` one of a set
//! (`[!...]` none of it, `a-z` a range), everything else itself, case and
//! all. A `[` with no `]` after it is a plain `[`.

#[derive(Debug, PartialEq)]
enum Token {
    Star,
    Any,
    Set { not: bool, items: Vec<(char, char)> },
    Char(char),
}

fn parse(pattern: &str) -> Vec<Token> {
    let p: Vec<char> = pattern.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < p.len() {
        let c = p[i];
        i += 1;
        match c {
            '*' => {
                // One star does what any run of them does.
                if out.last() != Some(&Token::Star) {
                    out.push(Token::Star);
                }
            }
            '?' => out.push(Token::Any),
            '[' => {
                // The set ends at the first `]` after an optional `!` and
                // a `]` that's part of it.
                let mut j = i;
                if j < p.len() && p[j] == '!' {
                    j += 1;
                }
                if j < p.len() && p[j] == ']' {
                    j += 1;
                }
                while j < p.len() && p[j] != ']' {
                    j += 1;
                }
                if j >= p.len() {
                    out.push(Token::Char('['));
                    continue;
                }
                let mut body = &p[i..j];
                i = j + 1;
                let not = body.first() == Some(&'!');
                if not {
                    body = &body[1..];
                }
                let mut items = Vec::new();
                let mut k = 0;
                while k < body.len() {
                    if k + 2 < body.len() && body[k + 1] == '-' {
                        items.push((body[k], body[k + 2]));
                        k += 3;
                    } else {
                        items.push((body[k], body[k]));
                        k += 1;
                    }
                }
                out.push(Token::Set { not, items });
            }
            c => out.push(Token::Char(c)),
        }
    }
    out
}

fn one(token: &Token, c: char) -> bool {
    match token {
        Token::Any => true,
        Token::Char(want) => *want == c,
        Token::Set { not, items } => items.iter().any(|&(lo, hi)| lo <= c && c <= hi) != *not,
        Token::Star => false,
    }
}

/// Whether `text` matches `pattern`, whole.
pub fn matches(pattern: &str, text: &str) -> bool {
    let tokens = parse(pattern);
    let text: Vec<char> = text.chars().collect();
    let (mut t, mut s) = (0, 0);
    // The last star seen, and where in the text its run ends for now.
    let mut star: Option<(usize, usize)> = None;
    while s < text.len() {
        match tokens.get(t) {
            Some(Token::Star) => {
                star = Some((t, s));
                t += 1;
            }
            Some(token) if one(token, text[s]) => {
                t += 1;
                s += 1;
            }
            _ => match star {
                // Let the last star take one more character, and go on from there.
                Some((st, ss)) => {
                    star = Some((st, ss + 1));
                    t = st + 1;
                    s = ss + 1;
                }
                None => return false,
            },
        }
    }
    tokens[t..].iter().all(|token| *token == Token::Star)
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn stars_and_questions() {
        assert!(matches("renovate/*", "renovate/x"));
        assert!(matches("renovate/*", "renovate/"));
        assert!(matches("renovate/*", "renovate/a/b"));
        assert!(!matches("renovate/*", "main"));
        assert!(!matches("renovate/*", "Renovate/x"));
        assert!(matches("flaky: *", "flaky: browser/stream[2]"));
        assert!(!matches("flaky: *", "not flaky: x"));
        assert!(matches("*", ""));
        assert!(matches("a*b*c", "aXbYbZc"));
        assert!(!matches("a*b*c", "aXbYbZ"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "ac"));
        assert!(matches("Dependency Dashboard", "Dependency Dashboard"));
        assert!(!matches(
            "Dependency Dashboard",
            "Dependency Dashboard (old)"
        ));
        assert!(!matches("Dependency Dashboard", "dependency dashboard"));
    }

    #[test]
    fn sets() {
        assert!(matches("v[0-9]", "v7"));
        assert!(!matches("v[0-9]", "vx"));
        assert!(matches("v[!0-9]", "vx"));
        assert!(!matches("v[!0-9]", "v7"));
        assert!(matches("[]a]", "]"));
        assert!(matches("[ab-]", "-"));
        // No closing bracket: a plain `[`.
        assert!(matches("a[b", "a[b"));
        assert!(!matches("a[b", "ab"));
    }
}
