//! Brace expansion, as bash does it: a word's unquoted `{…}` that holds an
//! unquoted `,` at its own level (`a{b,c}`) or is a sequence (`{a..c}`,
//! `{1..9..2}`) becomes one word per item, the text before and after it
//! joined on, items holding lists of their own expanded in turn. Any other
//! pair of braces is text (`{a}`, `{}`), though a list inside it still
//! expands (`{{a,b}}` is `{a}` and `{b}`), and so is a brace a quote or
//! backslash made text (`{"a,b"}`), which is why each character comes with
//! whether it was unquoted. A word it makes that is empty goes (`{,a}` is
//! `a`), unless that word itself holds a quote (`{,""}` is one empty word),
//! so where each quote was, even an empty one, goes with the characters.

use super::{DEPTH, TooDeep, UNKNOWN};

/// How many words one word may expand to, and how many characters they
/// may hold between them, before the reading gives up: far more than a
/// real command spells out.
pub const WORDS: usize = 4096;
const CHARS: usize = 1 << 22;

/// What one place in a word is.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// An unquoted character, which may make a brace expansion.
    Bare,
    /// Any other character.
    Text,
    /// Where a quote began: no character, but the word it's in stays even
    /// when empty.
    Quote,
}

/// A word's characters, each with what it is.
type Text = Vec<(char, Kind)>;

/// `word` expanded; `bare` holds the byte offset of each character of it
/// that was unquoted, and `quotes` where each quote in it began, in order.
pub fn expand(word: &str, bare: &[usize], quotes: &[usize]) -> Result<Vec<String>, TooDeep> {
    if !bare.iter().any(|&at| word.as_bytes()[at] == b'{') {
        return Ok(vec![word.to_owned()]);
    }
    let mut marks = bare.iter().peekable();
    let mut quotes = quotes.iter().peekable();
    let mut text = Text::new();
    for (at, c) in word.char_indices().chain([(word.len(), '\0')]) {
        while quotes.next_if(|&&q| q <= at).is_some() {
            text.push(('\0', Kind::Quote));
        }
        if at < word.len() {
            let bare = marks.next_if(|&&m| m == at).is_some();
            text.push((c, if bare { Kind::Bare } else { Kind::Text }));
        }
    }
    let braces = Braces::new(&text);
    Ok(braces
        .words(0, text.len(), 0)?
        .into_iter()
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.into_iter()
                .filter(|&(_, kind)| kind != Kind::Quote)
                .map(|(c, _)| c)
                .collect()
        })
        .collect())
}

/// A word's unquoted braces, paired as the shell pairs them.
struct Braces<'t> {
    text: &'t [(char, Kind)],
    /// For each `{`, where its `}` is, if it has one.
    close: Vec<Option<usize>>,
    /// For each `{`, the `,`s at its own level.
    commas: Vec<Vec<usize>>,
}

impl<'t> Braces<'t> {
    /// Each `}` closes the innermost `{` open; one with none open is text.
    /// A part of the word between a pair's braces, or after one, pairs the
    /// same way read alone, so the pairing is done once.
    fn new(text: &'t [(char, Kind)]) -> Braces<'t> {
        let mut close = vec![None; text.len()];
        let mut commas = vec![Vec::new(); text.len()];
        let mut open = Vec::new();
        for (at, &(c, kind)) in text.iter().enumerate() {
            match (c, kind) {
                ('{', Kind::Bare) => open.push(at),
                ('}', Kind::Bare) => {
                    if let Some(o) = open.pop() {
                        close[o] = Some(at);
                    }
                }
                (',', Kind::Bare) => {
                    if let Some(&o) = open.last() {
                        commas[o].push(at);
                    }
                }
                _ => {}
            }
        }
        Braces {
            text,
            close,
            commas,
        }
    }

    /// What `text[from..to]` expands to: each list or sequence in turn,
    /// left to right, the words so far taking each of its items.
    fn words(&self, from: usize, to: usize, depth: usize) -> Result<Vec<Text>, TooDeep> {
        if depth > DEPTH {
            return Err(TooDeep);
        }
        let mut out = vec![Vec::new()];
        let mut pos = from;
        while let Some((open, close, items)) = self.first_list(pos, to, depth)? {
            out = joined(out, &self.text[pos..open], &items)?;
            pos = close + 1;
        }
        joined(out, &self.text[pos..to], &[Vec::new()])
    }

    /// The first `{` in `text[from..to]` that opens a list or a sequence
    /// closed there too, its `}`, and its items, expanded. A `{` that opens
    /// neither is passed over, and one inside it may.
    #[allow(clippy::type_complexity)]
    fn first_list(
        &self,
        from: usize,
        to: usize,
        depth: usize,
    ) -> Result<Option<(usize, usize, Vec<Text>)>, TooDeep> {
        for open in from..to {
            let Some(close) = self.close[open].filter(|&c| c < to) else {
                continue;
            };
            if !self.commas[open].is_empty() {
                let bounds: Vec<usize> = std::iter::once(open)
                    .chain(self.commas[open].iter().copied())
                    .chain(std::iter::once(close))
                    .collect();
                let mut items = Vec::new();
                for item in bounds.windows(2) {
                    items.extend(self.words(item[0] + 1, item[1], depth + 1)?);
                    if items.len() > WORDS {
                        return Err(TooDeep);
                    }
                }
                return Ok(Some((open, close, items)));
            }
            if let Some(items) = sequence(&self.text[open + 1..close])? {
                return Ok(Some((open, close, items)));
            }
        }
        Ok(None)
    }
}

/// Each of `words` with `between` and then each of `items` joined on.
fn joined(
    words: Vec<Text>,
    between: &[(char, Kind)],
    items: &[Text],
) -> Result<Vec<Text>, TooDeep> {
    let mut out = Vec::with_capacity(words.len() * items.len());
    let mut chars = 0;
    for word in &words {
        for item in items {
            chars += word.len() + between.len() + item.len();
            if out.len() == WORDS || chars > CHARS {
                return Err(TooDeep);
            }
            out.push([word.as_slice(), between, item].concat());
        }
    }
    Ok(out)
}

/// `{x..y}` or `{x..y..step}`, all of it unquoted: integers, or two single
/// letters, which run through the characters between them. An integer
/// written with a leading zero pads every item to the longer end's width.
/// One bash would read as a sequence but the guard can't follow is too
/// deep, never text: an integer past 64 bits, a run whose length doesn't
/// fit them, or letters that run through `\` or `` ` ``, which bash then
/// reads as a quote or a substitution. A `[` a run makes is a glob, as
/// any unquoted one is.
fn sequence(inner: &[(char, Kind)]) -> Result<Option<Vec<Text>>, TooDeep> {
    if !inner.iter().all(|&(_, kind)| kind == Kind::Bare) {
        return Ok(None);
    }
    let inner: String = inner.iter().map(|&(c, _)| c).collect();
    let mut parts = inner.split("..");
    let (Some(from), Some(to)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let step = match parts.next().map(integer) {
        Some(Ok(Some(step))) => step,
        Some(Ok(None)) => return Ok(None),
        Some(Err(TooDeep)) => return Err(TooDeep),
        None => 1,
    };
    if parts.next().is_some() {
        return Ok(None);
    }
    let text = |s: String| s.chars().map(|c| (c, Kind::Text)).collect::<Text>();
    let letter = |s: &str| {
        let mut chars = s.chars();
        chars
            .next()
            .filter(|c| c.is_ascii_alphabetic() && chars.next().is_none())
    };
    if let (Some(a), Some(b)) = (letter(from), letter(to)) {
        return run(a as i64, b as i64, step)?
            .map(|n| match char::from(n as u8) {
                '\\' | '`' => Err(TooDeep),
                '[' => Ok(text(format!("{UNKNOWN}["))),
                c => Ok(text(c.to_string())),
            })
            .collect::<Result<_, _>>()
            .map(Some);
    }
    let (Some(a), Some(b)) = (integer(from)?, integer(to)?) else {
        return Ok(None);
    };
    let padded = |s: &str| {
        let digits = s.trim_start_matches('-');
        digits.len() > 1 && digits.starts_with('0')
    };
    let width = if padded(from) || padded(to) {
        from.len().max(to.len())
    } else {
        0
    };
    Ok(Some(
        run(a, b, step)?
            .map(|n| text(format!("{n:0width$}")))
            .collect(),
    ))
}

/// `s` as bash reads an integer: a sign or none, then digits. Not one is
/// `None`; one past 64 bits is too deep.
fn integer(s: &str) -> Result<Option<i64>, TooDeep> {
    let digits = s.strip_prefix(['+', '-']).unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    s.parse().map(Some).map_err(|_| TooDeep)
}

/// From `a` to `b` by `step`, either way; the step's sign doesn't count,
/// and a step of 0 is 1. A run whose span or step doesn't fit 64 bits,
/// which bash leaves as text, or that makes too many words, is too deep.
fn run(a: i64, b: i64, step: i64) -> Result<impl Iterator<Item = i64>, TooDeep> {
    b.checked_sub(a).ok_or(TooDeep)?;
    let step = step.checked_abs().ok_or(TooDeep)?.max(1);
    let count = a.abs_diff(b) / step as u64 + 1;
    if count > WORDS as u64 {
        return Err(TooDeep);
    }
    let step = if b < a { -step } else { step };
    Ok((0..count as i64).map(move |i| a + i * step))
}
