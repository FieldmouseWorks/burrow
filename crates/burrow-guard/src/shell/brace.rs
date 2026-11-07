//! Brace expansion, as bash does it: a word's unquoted `{…}` that holds an
//! unquoted `,` at its own level (`a{b,c}`) or is a sequence (`{a..c}`,
//! `{1..9..2}`) becomes one word per item, the text before and after it
//! joined on, items holding lists of their own expanded in turn. Any other
//! pair of braces is text (`{a}`, `{}`), though a list inside it still
//! expands (`{{a,b}}` is `{a}` and `{b}`), and so is a brace a quote or
//! backslash made text (`{"a,b"}`), which is why each character comes with
//! whether it was unquoted. Words it empties go (`{,a}` is `a`), unless
//! some of the word was quoted.

use super::{DEPTH, TooDeep};

/// How many words one word may expand to, and how many characters they
/// may hold between them, before the reading gives up: far more than a
/// real command spells out.
pub const WORDS: usize = 4096;
const CHARS: usize = 1 << 22;

/// The longest `{…}` read as a sequence: `{-9223372036854775808..0..2}`.
const SEQUENCE: usize = 48;

/// A word's characters, each with whether it was unquoted.
type Text = Vec<(char, bool)>;

/// `word` expanded; `bare` holds the byte offset of each character of it
/// that was unquoted, in order, and `quoted` says some of it was quoted.
pub fn expand(word: &str, bare: &[usize], quoted: bool) -> Result<Vec<String>, TooDeep> {
    if !bare.iter().any(|&at| word.as_bytes()[at] == b'{') {
        return Ok(vec![word.to_owned()]);
    }
    let mut marks = bare.iter().peekable();
    let text: Text = word
        .char_indices()
        .map(|(at, c)| (c, marks.next_if(|&&m| m == at).is_some()))
        .collect();
    let braces = Braces::new(&text);
    Ok(braces
        .words(0, text.len(), 0)?
        .into_iter()
        .map(|t| t.into_iter().map(|(c, _)| c).collect::<String>())
        .filter(|w| quoted || !w.is_empty())
        .collect())
}

/// A word's unquoted braces, paired as the shell pairs them.
struct Braces<'t> {
    text: &'t [(char, bool)],
    /// For each `{`, where its `}` is, if it has one.
    close: Vec<Option<usize>>,
    /// For each `{`, the `,`s at its own level.
    commas: Vec<Vec<usize>>,
}

impl<'t> Braces<'t> {
    /// Each `}` closes the innermost `{` open; one with none open is text.
    /// A part of the word between a pair's braces, or after one, pairs the
    /// same way read alone, so the pairing is done once.
    fn new(text: &'t [(char, bool)]) -> Braces<'t> {
        let mut close = vec![None; text.len()];
        let mut commas = vec![Vec::new(); text.len()];
        let mut open = Vec::new();
        for (at, &(c, bare)) in text.iter().enumerate() {
            match (c, bare) {
                ('{', true) => open.push(at),
                ('}', true) => {
                    if let Some(o) = open.pop() {
                        close[o] = Some(at);
                    }
                }
                (',', true) => {
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
    between: &[(char, bool)],
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
fn sequence(inner: &[(char, bool)]) -> Result<Option<Vec<Text>>, TooDeep> {
    if inner.len() > SEQUENCE || !inner.iter().all(|&(_, bare)| bare) {
        return Ok(None);
    }
    let inner: String = inner.iter().map(|&(c, _)| c).collect();
    let mut parts = inner.split("..");
    let (Some(from), Some(to)) = (parts.next(), parts.next()) else {
        return Ok(None);
    };
    let step = match parts.next().map(str::parse::<i64>) {
        Some(Ok(step)) => step.unsigned_abs().max(1),
        Some(Err(_)) => return Ok(None),
        None => 1,
    };
    if parts.next().is_some() {
        return Ok(None);
    }
    let plain = |s: String| s.chars().map(|c| (c, false)).collect::<Text>();
    let letter = |s: &str| {
        let mut chars = s.chars();
        chars
            .next()
            .filter(|c| c.is_ascii_alphabetic() && chars.next().is_none())
    };
    if let (Some(a), Some(b)) = (letter(from), letter(to)) {
        return Ok(Some(
            run(a as i64, b as i64, step)?
                .map(|n| plain(char::from(n as u8).to_string()))
                .collect(),
        ));
    }
    let (Ok(a), Ok(b)) = (from.parse::<i64>(), to.parse::<i64>()) else {
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
            .map(|n| plain(format!("{n:0width$}")))
            .collect(),
    ))
}

/// From `a` to `b` by `step`, either way.
fn run(a: i64, b: i64, step: u64) -> Result<impl Iterator<Item = i64>, TooDeep> {
    let count = a.abs_diff(b) / step + 1;
    if count > WORDS as u64 {
        return Err(TooDeep);
    }
    let step = if b < a { -(step as i128) } else { step as i128 };
    Ok((0..count as i128).map(move |i| (a as i128 + i * step) as i64))
}
