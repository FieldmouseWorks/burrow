//! burrow.backup.snapshots, a shell glob on a snapshot's name, as bash's
//! `[[ NAME == PATTERN ]]` matched it: `*`, `?`, `[...]` (a range, `!` or
//! `^` to negate, a POSIX class), `\` to take the next character as it is,
//! and extglob's groups, which `[[` always takes: `@(A|B)` one of them,
//! `?(…)` none or one, `*(…)` any number, `+(…)` one or more and `!(…)`
//! anything but one of them.

/// Whether `name` is one `pattern` matches.
pub fn matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    at(&p, &n)
}

fn at(p: &[char], n: &[char]) -> bool {
    let Some((&c, rest)) = p.split_first() else {
        return n.is_empty();
    };
    if rest.first() == Some(&'(') && opens(c) {
        return match group(&rest[1..]) {
            Some((alts, after)) => extended(c, &alts, after, n),
            // Never closed, the rest of the pattern is taken as it is.
            None => p == n,
        };
    }
    match c {
        '*' => (0..=n.len()).any(|i| at(rest, &n[i..])),
        '?' => !n.is_empty() && at(rest, &n[1..]),
        '[' => match class(rest) {
            Some((set, after)) => n
                .split_first()
                .is_some_and(|(&x, more)| set.has(x) && at(after, more)),
            // A `[` that's never closed is a `[`.
            None => n.first() == Some(&'[') && at(rest, &n[1..]),
        },
        '\\' => match rest.split_first() {
            Some((&x, after)) => n.first() == Some(&x) && at(after, &n[1..]),
            None => n == ['\\'],
        },
        c => n.first() == Some(&c) && at(rest, &n[1..]),
    }
}

/// Whether `c` before a `(` starts an extglob group.
fn opens(c: char) -> bool {
    matches!(c, '@' | '?' | '*' | '+' | '!')
}

/// A group after its `(`: its patterns, split at the `|`s outside any
/// parentheses in it, and what follows its `)`; None when it's never closed.
/// As bash scans one: a `\` takes the next character, a bracket expression
/// is passed over whole, and every `(` opens another level.
fn group(p: &[char]) -> Option<(Vec<&[char]>, &[char])> {
    let (mut alts, mut start, mut depth, mut i) = (Vec::new(), 0, 0, 0);
    while i < p.len() {
        match p[i] {
            '\\' => i += 1,
            // One that's never closed takes the rest, the `)` with it.
            '[' => {
                let (_, after) = class(&p[i + 1..])?;
                i = p.len() - after.len();
                continue;
            }
            '(' => depth += 1,
            ')' if depth == 0 => {
                alts.push(&p[start..i]);
                return Some((alts, &p[i + 1..]));
            }
            ')' => depth -= 1,
            '|' if depth == 0 => {
                alts.push(&p[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Whether `n` is a group of `kind` with patterns `alts`, then `rest`.
fn extended(kind: char, alts: &[&[char]], rest: &[char], n: &[char]) -> bool {
    let one = |s: &[char]| alts.iter().any(|a| at(a, s));
    (0..=n.len()).any(|i| {
        let (head, tail) = n.split_at(i);
        let fits = match kind {
            '@' => one(head),
            '?' => head.is_empty() || one(head),
            '*' => repeated(alts, head),
            '+' if head.is_empty() => one(head),
            '+' => (1..=head.len()).any(|j| one(&head[..j]) && repeated(alts, &head[j..])),
            _ => !one(head),
        };
        fits && at(rest, tail)
    })
}

/// Whether `s` is any number of `alts`' matches, one after another.
fn repeated(alts: &[&[char]], s: &[char]) -> bool {
    s.is_empty()
        || (1..=s.len()).any(|j| alts.iter().any(|a| at(a, &s[..j])) && repeated(alts, &s[j..]))
}

struct Set {
    negated: bool,
    items: Vec<Item>,
}

enum Item {
    One(char),
    Range(char, char),
    Class(fn(char) -> bool),
}

impl Set {
    fn has(&self, x: char) -> bool {
        let found = self.items.iter().any(|i| match *i {
            Item::One(c) => c == x,
            Item::Range(a, b) => a <= x && x <= b,
            Item::Class(f) => f(x),
        });
        found != self.negated
    }
}

/// A bracket expression after its `[`: the set, and what follows its `]`.
fn class(p: &[char]) -> Option<(Set, &[char])> {
    let mut i = 0;
    let negated = matches!(p.first(), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let c = *p.get(i)?;
        if c == ']' && !first {
            return Some((Set { negated, items }, &p[i + 1..]));
        }
        first = false;
        if c == '[' && p.get(i + 1) == Some(&':') {
            let name: String = p[i + 2..].iter().take_while(|&&c| c != ':').collect();
            let end = i + 2 + name.chars().count();
            if p.get(end) == Some(&':')
                && p.get(end + 1) == Some(&']')
                && let Some(f) = posix(&name)
            {
                items.push(Item::Class(f));
                i = end + 2;
                continue;
            }
        }
        let (c, next) = member(p, i)?;
        if p.get(next) == Some(&'-') && p.get(next + 1).is_some_and(|&e| e != ']') {
            let (end, after) = member(p, next + 1)?;
            items.push(Item::Range(c, end));
            i = after;
        } else {
            items.push(Item::One(c));
            i = next;
        }
    }
}

/// The character a bracket expression has at `i`, its `\\` taken off, and
/// where what follows it starts; None when the pattern ends on the `\\`.
fn member(p: &[char], i: usize) -> Option<(char, usize)> {
    match *p.get(i)? {
        '\\' => Some((*p.get(i + 1)?, i + 2)),
        c => Some((c, i + 1)),
    }
}

fn posix(name: &str) -> Option<fn(char) -> bool> {
    Some(match name {
        "alnum" => |c: char| c.is_alphanumeric(),
        "alpha" => |c: char| c.is_alphabetic(),
        "blank" => |c: char| c == ' ' || c == '\t',
        "cntrl" => |c: char| c.is_control(),
        "digit" => |c: char| c.is_ascii_digit(),
        "graph" => |c: char| !c.is_whitespace() && !c.is_control(),
        "lower" => |c: char| c.is_lowercase(),
        "print" => |c: char| !c.is_control(),
        "punct" => |c: char| c.is_ascii_punctuation(),
        "space" => |c: char| c.is_whitespace(),
        "upper" => |c: char| c.is_uppercase(),
        "xdigit" => |c: char| c.is_ascii_hexdigit(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn a_glob_matches_as_bash_matches_it() {
        for (pattern, yes, no) in [
            (
                "autosnap_*_hourly",
                &["autosnap_2026-11-01_06:00:00_hourly", "autosnap__hourly"][..],
                &[
                    "autosnap_2026-11-01_daily",
                    "xautosnap_1_hourly",
                    "autosnap_1_hourlyx",
                ][..],
            ),
            ("snap-*", &["snap-1", "snap-"], &["snap", "Snap-1"]),
            ("snap-?", &["snap-1"], &["snap-", "snap-12"]),
            ("*", &["", "anything"], &[]),
            ("a*b*c", &["abc", "aXbYc", "abbc"], &["ab", "acb"]),
            (
                "snap-[0-9]",
                &["snap-0", "snap-4", "snap-9"],
                &["snap-a", "snap-"],
            ),
            ("snap-[!0-9]", &["snap-a"], &["snap-4"]),
            ("snap-[^0-9]", &["snap-a"], &["snap-4"]),
            ("x[]a]", &["x]", "xa"], &["xb"]),
            ("x[!]]", &["xa"], &["x]"]),
            ("x[a-]", &["xa", "x-"], &["xb"]),
            ("x[[:digit:]]y", &["x5y"], &["xay"]),
            ("x[[:upper:][:digit:]]", &["xA", "x1"], &["xa"]),
            ("a\\*", &["a*"], &["ab"]),
            ("a[\\]]", &["a]"], &["a\\"]),
            // A range's endpoints, either escaped, with what follows them.
            ("snap-[a-z]", &["snap-b"], &["snap-A", "snap-z]"]),
            (
                "snap-[a-\\z]",
                &["snap-a", "snap-b", "snap-z"],
                &["snap-\\", "snap-A", "snap-]"],
            ),
            ("snap-[\\a-z]", &["snap-b"], &["snap-A"]),
            ("snap-[\\a-\\z]", &["snap-b"], &["snap-A", "snap-\\"]),
            ("snap-[z-\\a]", &[], &["snap-a", "snap-z", "snap-m"]),
            (
                "snap-[!a-\\z]",
                &["snap-A", "snap-\\"],
                &["snap-b", "snap-z"],
            ),
            ("snap-[^a-\\z]", &["snap-A"], &["snap-b"]),
            (
                "snap-[A-\\\\]",
                &["snap-Z", "snap-[", "snap-\\"],
                &["snap-]", "snap-a"],
            ),
            ("snap-[a-\\]]", &[], &["snap-a", "snap-]", "snap-b]"]),
            (
                "snap-[\\]-a]",
                &["snap-]", "snap-^", "snap-a"],
                &["snap-b", "snap-\\"],
            ),
            ("snap-[+-\\-]", &["snap-+", "snap--"], &["snap-a"]),
            (
                "snap-[a-\\cx]",
                &["snap-a", "snap-c", "snap-x"],
                &["snap-d", "snap-\\"],
            ),
            ("snap-[a-\\c\\x]", &["snap-b", "snap-x"], &["snap-d"]),
            (
                "snap-[a-\\cxy-z]",
                &["snap-b", "snap-x", "snap-z"],
                &["snap-d"],
            ),
            ("snap-[a-\\c-]", &["snap-b", "snap--"], &["snap-d"]),
            ("snap-[a-\\z]y", &["snap-by"], &["snap-b", "snap-\\y"]),
            ("snap-[a-\\z]*", &["snap-b", "snap-abc"], &["snap-A"]),
            ("[a-\\z]-[0-\\9]", &["b-5", "a-0", "z-9"], &["snap-b"]),
            // Never closed: the `\` takes what would've closed it.
            (
                "snap-[a-\\]",
                &["snap-[a-]"],
                &["snap-a", "snap-]", "snap-[a-\\]"],
            ),
            ("snap-[a-\\", &["snap-[a-\\"], &["snap-a"]),
            // In a group, whichever kind.
            ("@(snap-[a-\\z]|other)", &["snap-b", "other"], &["snap-A"]),
            ("@(other|snap-[!a-\\z])", &["snap-A", "other"], &["snap-b"]),
            ("?(snap-[a-\\z])", &["snap-b", ""], &["snap-A"]),
            ("*(snap-[a-\\z])", &["snap-b", ""], &["snap-A"]),
            ("+(snap-[a-\\z])", &["snap-b"], &["snap-A", ""]),
            ("!(snap-[a-\\z])", &["snap-A", ""], &["snap-b"]),
            ("snap-@([a-\\)]|0)", &[], &["snap-a", "snap-b", "snap-)"]),
            (
                "snap-@([\\(-\\)]|0)",
                &["snap-(", "snap-)", "snap-0"],
                &["snap-a", "snap-\\"],
            ),
            ("snap-@([a-\\]]|0)", &["snap-0"], &["snap-a", "snap-]"]),
            (
                "snap-+([a-\\c])",
                &["snap-ab", "snap-abc"],
                &["snap-", "snap-a-"],
            ),
            (
                "snap-@([a-\\c]@([x-\\z]|0))",
                &["snap-bx", "snap-b0"],
                &["snap-b", "snap-ab"],
            ),
            ("a[", &["a["], &["a"]),
            ("a[b", &["a[b"], &["ab"]),
            ("a\\", &["a\\"], &["a"]),
            (
                "autosnap_*_@(hourly|daily)",
                &["autosnap_1_hourly", "autosnap_1_daily"],
                &["autosnap_1_monthly", "autosnap_1_"],
            ),
            ("snap-?(x)y", &["snap-y", "snap-xy"], &["snap-xxy"]),
            ("a*(bc)d", &["ad", "abcd", "abcbcd"], &["abd", "abcbd"]),
            ("a+(bc)d", &["abcd", "abcbcd"], &["ad", "abcbd"]),
            ("+(a|ab)b", &["ab", "aab", "abb", "abab"], &["a", "b"]),
            ("+()", &[""], &["a"]),
            ("!(*hourly)", &["snap_daily", ""], &["snap_hourly"]),
            ("!(a)b", &["b", "bb"], &["ab"]),
            ("!(a|b)", &["", "ab", "c"], &["a", "b"]),
            ("@(a|b@(c|d))", &["a", "bc", "bd"], &["b", "ac"]),
            ("x@([)]|y)", &["x)", "xy"], &["x", "x]", "x)]"]),
            ("x@(a\\)|b)", &["xa)", "xb"], &["xa", "x)"]),
            ("@(a(b)|c)", &["a(b)", "c"], &["a(b", "a"]),
            // Never closed: the rest of the pattern as it is.
            ("@(a", &["@(a"], &["a"]),
            ("*(a", &["*(a"], &["(a", "x(a"]),
            ("x*(a?", &["x*(a?"], &["x*(ab"]),
            ("@(a(b)", &["@(a(b)"], &["a(b"]),
            ("@([)|a)", &["@([)|a)"], &["[", "a"]),
            ("\\@(a)", &["@(a)"], &["a"]),
        ] {
            for name in yes {
                assert!(matches(pattern, name), "{pattern} should match {name}");
            }
            for name in no {
                assert!(!matches(pattern, name), "{pattern} shouldn't match {name}");
            }
        }
    }
}
