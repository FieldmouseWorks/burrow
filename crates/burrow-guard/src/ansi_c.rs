//! A `$'…'` string's escapes, decoded as bash decodes them: `\nnn` and
//! `\xHH` are one byte each, cut to eight bits, and a NUL from any escape
//! ends the string, whatever is left of it.

use std::iter::Peekable;

/// Stands for what can't be held in text and can't matter to a reading that
/// only looks for ASCII words: a byte above ASCII on its own, or a code
/// point that isn't a character.
const OTHER: char = char::REPLACEMENT_CHARACTER;

/// A byte's text, or `None` for a NUL.
fn byte(n: u32) -> Option<char> {
    match n & 0xff {
        0 => None,
        n if n < 0x80 => char::from_u32(n),
        _ => Some(OTHER),
    }
}

/// A code point's text, or `None` for a NUL.
fn unicode(n: u32) -> Option<char> {
    (n != 0).then(|| char::from_u32(n).unwrap_or(OTHER))
}

/// `first` followed by up to `most` more digits in `radix`, as a number.
fn digits(
    rest: &mut Peekable<impl Iterator<Item = char>>,
    first: u32,
    radix: u32,
    most: usize,
) -> u32 {
    let mut n = first;
    for _ in 0..most {
        let Some(d) = rest.peek().and_then(|c| c.to_digit(radix)) else {
            break;
        };
        n = n.wrapping_mul(radix).wrapping_add(d);
        rest.next();
    }
    n
}

/// What's between a `$'…'`'s quotes, as text, appended to `out`.
pub fn decode(raw: &[char], out: &mut String) {
    let mut rest = raw.iter().copied().peekable();
    while let Some(c) = rest.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let Some(e) = rest.next() else {
            out.push('\\');
            return;
        };
        let decoded = match e {
            'a' => Some('\x07'),
            'b' => Some('\x08'),
            'e' | 'E' => Some('\x1b'),
            'f' => Some('\x0c'),
            'n' => Some('\n'),
            'r' => Some('\r'),
            't' => Some('\t'),
            'v' => Some('\x0b'),
            '\\' | '\'' | '"' | '?' => Some(e),
            '0'..='7' => byte(digits(&mut rest, e.to_digit(8).unwrap_or(0), 8, 2)),
            'x' | 'u' | 'U' => {
                let most = match e {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                let brace = e == 'x' && rest.peek() == Some(&'{');
                if brace {
                    rest.next();
                    let n = digits(&mut rest, 0, 16, usize::MAX);
                    if rest.peek() == Some(&'}') {
                        rest.next();
                    }
                    byte(n)
                } else if rest.peek().is_some_and(char::is_ascii_hexdigit) {
                    let n = digits(&mut rest, 0, 16, most);
                    if e == 'x' { byte(n) } else { unicode(n) }
                } else {
                    out.push('\\');
                    Some(e)
                }
            }
            'c' => match rest.next() {
                None => {
                    out.push('\\');
                    Some('c')
                }
                Some('?') => Some('\x7f'),
                Some(c) => {
                    // A control of a backslash is written with both of them.
                    if c == '\\' && rest.peek() == Some(&'\\') {
                        rest.next();
                    }
                    if c.is_ascii() {
                        byte(u32::from(c) & 0x1f)
                    } else {
                        Some(OTHER)
                    }
                }
            },
            e => {
                out.push('\\');
                Some(e)
            }
        };
        let Some(decoded) = decoded else { return };
        out.push(decoded);
    }
}

#[cfg(test)]
mod tests {
    use super::{OTHER, decode};

    fn read(raw: &str) -> String {
        let mut out = String::new();
        decode(&raw.chars().collect::<Vec<_>>(), &mut out);
        out
    }

    /// Each answer here is what bash 5.3 gives for `$'…'`.
    #[test]
    fn escapes_decode_as_bash_decodes_them() {
        for (raw, want) in [
            (r"\a\b\e\E\f\n\r\t\v", "\x07\x08\x1b\x1b\x0c\n\r\t\x0b"),
            (r#"\\\'\"\?"#, "\\'\"?"),
            // Unknown escapes stay as they are.
            (r"\q\8\xZ\uZ\UZ", r"\q\8\xZ\uZ\UZ"),
            (r"\c", r"\c"),
            (r"a\", r"a\"),
            // Three octal digits at most, cut to a byte.
            (r"\1234", "S4"),
            (r"git\440push", "git push"),
            (r"\x2", "\x02"),
            (r"\x41B", "AB"),
            (r"\x{20}a", " a"),
            (r"\x{120}a", " a"),
            (r"\u0020", " "),
            (r"\U00000020x", " x"),
            // Controls.
            (r"\ca\cA", "\x01\x01"),
            (r"\c?", "\x7f"),
            (r"\c\\x", "\x1cx"),
        ] {
            assert_eq!(read(raw), want, "{raw}");
        }
    }

    #[test]
    fn a_nul_from_any_escape_ends_the_string() {
        for raw in [
            r"a\0b",
            r"a\000b",
            r"a\400b",
            r"a\x00b",
            r"a\x{}b",
            r"a\x{100}b",
            r"a\u0000b",
            r"a\U00000000b",
            r"a\c@b",
        ] {
            assert_eq!(read(raw), "a", "{raw}");
        }
    }

    #[test]
    fn what_text_cant_hold_is_one_other_character() {
        for raw in [r"\777", r"\xc3", r"\uD800", r"\U110000"] {
            assert_eq!(read(raw), OTHER.to_string(), "{raw}");
        }
        assert_eq!(read(r"\u00e9"), "é");
    }
}
