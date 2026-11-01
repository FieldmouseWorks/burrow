//! An exclude pattern, read as bash's `[[ NAME == PATTERN ]]` reads it, so a
//! machine's `exclude` means what it always has: `*` any run of characters,
//! `/` included, `?` any one, `[...]` a set (`!` or `^` negates it, with
//! ranges, `[:classes:]`, `[=c=]`, `[.c.]` and `\` escapes), the extended
//! `?(a|b)`, `*(...)`, `+(...)`, `@(...)` and `!(...)`, `\` an escape, and
//! everything else itself, case and all. What isn't closed is plain text.
//! The tests hold it to bash itself.

#[derive(Debug)]
enum Node {
    Char(char),
    Any,
    Star,
    Set { not: bool, items: Vec<Item> },
    Ext { kind: char, alts: Vec<Vec<Node>> },
}

#[derive(Debug)]
enum Item {
    Range(char, char),
    Class(String),
}

/// Whether `name` matches `pattern`.
pub fn matches(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = name.chars().collect();
    walk(&parse(&p), &s)
}

fn parse(p: &[char]) -> Vec<Node> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < p.len() {
        let c = p[i];
        if matches!(c, '?' | '*' | '+' | '@' | '!')
            && p.get(i + 1) == Some(&'(')
            && let Some((alts, end)) = alternatives(p, i + 2)
        {
            let alts = alts.iter().map(|a| parse(a)).collect();
            out.push(Node::Ext { kind: c, alts });
            i = end;
            continue;
        }
        i += 1;
        match c {
            '*' => out.push(Node::Star),
            '?' => out.push(Node::Any),
            '[' => match set(p, i) {
                Some((node, end)) => {
                    out.push(node);
                    i = end;
                }
                None => out.push(Node::Char('[')),
            },
            '\\' if i < p.len() => {
                out.push(Node::Char(p[i]));
                i += 1;
            }
            c => out.push(Node::Char(c)),
        }
    }
    out
}

/// What's between the parens that open before `start`, split at its own
/// `|`s, and where it ends. None when nothing closes them.
fn alternatives(p: &[char], start: usize) -> Option<(Vec<&[char]>, usize)> {
    let mut alts = Vec::new();
    let mut from = start;
    let mut depth = 1;
    let mut i = start;
    while i < p.len() {
        match p[i] {
            '\\' => i += 1,
            '[' => {
                if let Some((_, end)) = set(p, i + 1) {
                    i = end - 1;
                }
            }
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    alts.push(&p[from..i]);
                    return Some((alts, i + 1));
                }
            }
            '|' if depth == 1 => {
                alts.push(&p[from..i]);
                from = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// The set whose body starts at `i`, just after its `[`, and where it ends.
/// None when no `]` closes it.
fn set(p: &[char], mut i: usize) -> Option<(Node, usize)> {
    let not = matches!(p.get(i), Some('!' | '^'));
    if not {
        i += 1;
    }
    let mut items = Vec::new();
    let mut first = true;
    loop {
        let c = *p.get(i)?;
        if c == ']' && !first {
            return Some((Node::Set { not, items }, i + 1));
        }
        first = false;
        if let Some((item, end)) = bracketed(p, i) {
            i = end;
            match item {
                Item::Range(lo, _) if p.get(i) == Some(&'-') && has_end(p, i + 1) => {
                    let (hi, end) = member(p, i + 1)?;
                    items.push(Item::Range(lo, hi));
                    i = end;
                }
                item => items.push(item),
            }
            continue;
        }
        let (lo, end) = member(p, i)?;
        i = end;
        if p.get(i) == Some(&'-') && has_end(p, i + 1) {
            let (hi, end) = member(p, i + 1)?;
            items.push(Item::Range(lo, hi));
            i = end;
        } else {
            items.push(Item::Range(lo, lo));
        }
    }
}

/// Whether a range's `-` has an end at `i`: before a `]`, it's a plain `-`.
fn has_end(p: &[char], i: usize) -> bool {
    p.get(i).is_some_and(|c| *c != ']')
}

/// A `[:class:]`, `[=c=]` or `[.c.]` at `i`, and where it ends.
fn bracketed(p: &[char], i: usize) -> Option<(Item, usize)> {
    if p.get(i) != Some(&'[') {
        return None;
    }
    let mark = *p.get(i + 1)?;
    if !matches!(mark, ':' | '=' | '.') {
        return None;
    }
    let body = i + 2;
    let len = p[body..]
        .windows(2)
        .position(|w| w[0] == mark && w[1] == ']')?;
    let text = &p[body..body + len];
    let item = match (mark, text) {
        (':', _) => Item::Class(text.iter().collect()),
        (_, [c]) => Item::Range(*c, *c),
        _ => return None,
    };
    Some((item, body + len + 2))
}

/// One character of a set at `i`, escaped or not, and where it ends.
fn member(p: &[char], i: usize) -> Option<(char, usize)> {
    match *p.get(i)? {
        '\\' => Some((*p.get(i + 1)?, i + 2)),
        c => Some((c, i + 1)),
    }
}

fn in_class(class: &str, c: char) -> bool {
    match class {
        "alnum" => c.is_ascii_alphanumeric(),
        "alpha" => c.is_ascii_alphabetic(),
        "ascii" => c.is_ascii(),
        "blank" => c == ' ' || c == '\t',
        "cntrl" => c.is_ascii_control(),
        "digit" => c.is_ascii_digit(),
        "graph" => c.is_ascii_graphic(),
        "lower" => c.is_ascii_lowercase(),
        "print" => c.is_ascii_graphic() || c == ' ',
        "punct" => c.is_ascii_punctuation(),
        "space" => c.is_ascii_whitespace() || c == '\x0b',
        "upper" => c.is_ascii_uppercase(),
        "word" => c.is_ascii_alphanumeric() || c == '_',
        "xdigit" => c.is_ascii_hexdigit(),
        _ => false,
    }
}

fn walk(nodes: &[Node], s: &[char]) -> bool {
    let Some((node, rest)) = nodes.split_first() else {
        return s.is_empty();
    };
    let one_then_rest =
        |c: &dyn Fn(char) -> bool| s.first().is_some_and(|f| c(*f)) && walk(rest, &s[1..]);
    match node {
        Node::Char(c) => one_then_rest(&|f| f == *c),
        Node::Any => one_then_rest(&|_| true),
        Node::Set { not, items } => one_then_rest(&|f| {
            let found = items.iter().any(|item| match item {
                Item::Range(lo, hi) => (*lo..=*hi).contains(&f),
                Item::Class(class) => in_class(class, f),
            });
            found != *not
        }),
        Node::Star => (0..=s.len()).any(|k| walk(rest, &s[k..])),
        Node::Ext { kind, alts } => match kind {
            '@' => once(alts, rest, s),
            '?' => walk(rest, s) || once(alts, rest, s),
            '*' => repeats(alts, rest, s),
            '+' => (0..=s.len()).any(|k| any_whole(alts, &s[..k]) && repeats(alts, rest, &s[k..])),
            _ => (0..=s.len()).any(|k| !any_whole(alts, &s[..k]) && walk(rest, &s[k..])),
        },
    }
}

fn any_whole(alts: &[Vec<Node>], s: &[char]) -> bool {
    alts.iter().any(|a| walk(a, s))
}

fn once(alts: &[Vec<Node>], rest: &[Node], s: &[char]) -> bool {
    (0..=s.len()).any(|k| any_whole(alts, &s[..k]) && walk(rest, &s[k..]))
}

/// Any number of the alternatives, then the rest. A turn that takes nothing
/// gets no further, so each one takes something.
fn repeats(alts: &[Vec<Node>], rest: &[Node], s: &[char]) -> bool {
    walk(rest, s) || (1..=s.len()).any(|k| any_whole(alts, &s[..k]) && repeats(alts, rest, &s[k..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const NAMES: &[&str] = &[
        "sam/a",
        "sam/b",
        "sam/ab",
        "sam/abc",
        "sam/a-b",
        "sam/a.b",
        "sam/A",
        "sam/7",
        "sam/_x",
        "sam/",
        "kim/a",
        "kim/sam/a",
        "Sam/a",
        "sam/*",
        "sam/?",
        "sam/[a]",
        "sam/a\\b",
        "sam/(a)",
        "sam/a|b",
        "sam/^",
        "sam/!",
        "sam/]",
        "sam/-",
    ];

    const PATTERNS: &[&str] = &[
        "sam/a",
        "SAM/a",
        "sam/*",
        "*/a",
        "*",
        "**",
        "sam/?",
        "sam/??",
        "sam/[ab]",
        "sam/[^a]*",
        "sam/[!a]*",
        "sam/[^a-b]",
        "sam/[a-c]*",
        "sam/[c-a]",
        "sam/[]a]",
        "sam/[^]a]",
        "sam/[a-]",
        "sam/[-a]",
        "sam/[a\\]]",
        "sam/[\\^a]",
        "sam/[a^]",
        "sam/[!!]",
        "sam/[[:alpha:]]",
        "sam/[^[:alpha:]]",
        "sam/[[:digit:]]",
        "sam/[[:upper:]]",
        "sam/[[:lower:]]*",
        "sam/[[:alnum:]_]*",
        "sam/[[:punct:]]",
        "sam/[[:word:]]*",
        "sam/[[:xdigit:]]",
        "sam/[[:nosuch:]]",
        "sam/[[:nosuch:]a]",
        "sam/[[=a=]]",
        "sam/[[.a.]]",
        "sam/[[:alpha:]",
        "sam/[a",
        "sam/[",
        "sam/[]",
        "sam/[!]",
        "sam/\\*",
        "sam/\\?",
        "sam/\\[a\\]",
        "sam/a\\\\b",
        "sam/\\a",
        "sam/a\\",
        "sam/?(a)",
        "sam/?(a|b)",
        "sam/a?(b)",
        "sam/a?(b)?(c)",
        "sam/*(a)",
        "sam/*(a|b)",
        "sam/*(ab)c",
        "sam/+(a)",
        "sam/+(a|b)",
        "sam/+(a|b)c",
        "sam/@(a|b)",
        "sam/@(a|ab)c",
        "sam/@(a*)",
        "sam/!(a)",
        "sam/!(a|b)",
        "sam/!(a*)",
        "sam/!(a)b",
        "!(sam/*)",
        "@(sam|kim)/a",
        "sam/@(a|@(b|ab))",
        "sam/@(|a)",
        "sam/@()",
        "sam/@([ab)]|x)",
        "sam/@(\\(a\\))",
        "sam/@(a\\|b)",
        "sam/@(a",
        "sam/?(a",
        "sam/*(a",
        "sam/!(a",
        "sam/+(a",
        "sam/(a)",
        "sam/a|b",
        "sam/@(a(b)|a)",
        "sam/^",
        "sam/!",
    ];

    /// Every pattern against every name, as bash answers.
    #[test]
    fn answers_as_bash_does() {
        let loop_over = r#"n=$1; shift; names=("${@:1:n}"); patterns=("${@:n+1}")
            for p in "${patterns[@]}"; do for r in "${names[@]}"; do
              if [[ $r == $p ]]; then echo 1; else echo 0; fi
            done; done"#;
        let out = Command::new("bash")
            .args(["-c", loop_over, "bash"])
            .arg(NAMES.len().to_string())
            .args(NAMES)
            .args(PATTERNS)
            .env("LC_ALL", "C")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let said = String::from_utf8(out.stdout).unwrap();
        let mut said = said.lines();
        let mut wrong = Vec::new();
        for p in PATTERNS {
            for name in NAMES {
                let bash = said.next().unwrap() == "1";
                if matches(p, name) != bash {
                    wrong.push(format!("{name} == {p}: bash says {bash}"));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    /// The same without bash to compare with, and what the bash read gave
    /// that a plain glob's wouldn't.
    #[test]
    fn a_negated_set_excludes_what_it_doesnt_name() {
        assert!(matches("sam/[^a]*", "sam/b"));
        assert!(!matches("sam/[^a]*", "sam/a"));
        assert!(matches("sam/@(a|b)", "sam/b"));
        assert!(!matches("sam/!(a)", "sam/a"));
        assert!(matches("sam/[[:digit:]]", "sam/7"));
        assert!(matches("sam/\\*", "sam/*"));
        assert!(!matches("sam/\\*", "sam/a"));
    }
}
