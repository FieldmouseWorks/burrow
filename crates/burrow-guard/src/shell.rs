//! A command line read as the shell reads it, far enough to know what each
//! command it runs is given. Quotes (`'…'`, `"…"`, `$'…'`, `$"…"`) and
//! backslashes join and escape, and an unquoted `;`, `&`, `|`, `(`, `)` or
//! newline ends a command. A redirect and its target aren't words. A
//! substitution (`$(…)`, `` `…` ``, `<(…)`, `>(…)`) is read as commands of
//! its own wherever it is, inside double quotes or a redirect's target too,
//! and stands in its word as [`UNKNOWN`]. Nothing else is expanded.

/// What a substitution's output is, in the word that holds it: unknown
/// until it runs.
pub const UNKNOWN: char = '\u{FFFC}';

/// How deeply substitutions may nest before the reading gives up.
const DEPTH: usize = 64;

/// A command line that nests deeper than the guard reads.
#[derive(Debug, PartialEq, Eq)]
pub struct TooDeep;

/// Each command in `text` as its words, a substitution's commands among
/// them, ahead of the command that holds them.
pub fn commands(text: &str) -> Result<Vec<Vec<String>>, TooDeep> {
    let mut lexer = Lexer::new(text);
    lexer.list(0, false)?;
    Ok(lexer.commands)
}

struct Lexer {
    chars: Vec<char>,
    pos: usize,
    /// Every command read, from every scope.
    commands: Vec<Vec<String>>,
}

/// The command being read in one scope: the text, or one substitution.
#[derive(Default)]
struct Command {
    words: Vec<String>,
    word: String,
    /// A word has begun, even an empty one (`''`).
    in_word: bool,
    /// Some of the word was quoted or escaped.
    quoted: bool,
    /// The next word is a redirect's target.
    target: bool,
}

impl Command {
    fn push(&mut self, c: char) {
        self.word.push(c);
        self.in_word = true;
    }

    fn quote(&mut self) {
        self.in_word = true;
        self.quoted = true;
    }

    fn end_word(&mut self) {
        if self.in_word {
            let word = std::mem::take(&mut self.word);
            if !std::mem::take(&mut self.target) {
                self.words.push(word);
            }
        }
        self.in_word = false;
        self.quoted = false;
    }

    fn end(&mut self, commands: &mut Vec<Vec<String>>) {
        self.end_word();
        // A redirect with no target before the command ends takes nothing
        // from the next command.
        self.target = false;
        if !self.words.is_empty() {
            commands.push(std::mem::take(&mut self.words));
        }
    }
}

impl Lexer {
    fn new(text: &str) -> Lexer {
        Lexer {
            chars: text.chars().collect(),
            pos: 0,
            commands: Vec::new(),
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<char> {
        let c = self.peek()?;
        self.pos += 1;
        Some(c)
    }

    fn next_if(&mut self, f: impl Fn(char) -> bool) -> Option<char> {
        let c = self.peek().filter(|&c| f(c))?;
        self.pos += 1;
        Some(c)
    }

    /// Reads commands to the end of the text or, in a substitution
    /// (`closes`), to the `)` that ends it.
    fn list(&mut self, depth: usize, closes: bool) -> Result<(), TooDeep> {
        if depth > DEPTH {
            return Err(TooDeep);
        }
        let mut cmd = Command::default();
        // Subshells open in this scope: a `)` closes one of them first.
        let mut open = 0usize;
        while let Some(c) = self.next() {
            match c {
                ' ' | '\t' | '\r' => cmd.end_word(),
                ')' if closes && open == 0 => break,
                '(' => {
                    open += 1;
                    cmd.end(&mut self.commands);
                }
                ')' => {
                    open = open.saturating_sub(1);
                    cmd.end(&mut self.commands);
                }
                // `&>` and `&>>` send both outputs: a redirect, not the end.
                '&' if self.peek() == Some('>') => self.redirect(&mut cmd),
                '\n' | ';' | '&' | '|' => cmd.end(&mut self.commands),
                '<' | '>' if self.next_if(|c| c == '(').is_some() => {
                    self.list(depth + 1, true)?;
                    cmd.push(UNKNOWN);
                }
                '<' | '>' => self.redirect(&mut cmd),
                c => self.word(c, &mut cmd, depth)?,
            }
        }
        cmd.end(&mut self.commands);
        Ok(())
    }

    /// A character of a word, outside any quotes.
    fn word(&mut self, c: char, cmd: &mut Command, depth: usize) -> Result<(), TooDeep> {
        match c {
            '\\' => match self.next() {
                Some('\n') | None => {}
                Some(c) => cmd.push(c),
            },
            '\'' => {
                cmd.quote();
                while let Some(c) = self.next() {
                    if c == '\'' {
                        break;
                    }
                    cmd.word.push(c);
                }
            }
            '"' => {
                cmd.quote();
                self.double(cmd, depth)?;
            }
            '`' => {
                self.backquote(depth, false)?;
                cmd.push(UNKNOWN);
            }
            '$' => match self.peek() {
                Some('\'') => {
                    self.pos += 1;
                    cmd.quote();
                    self.ansi_c(cmd);
                }
                Some('"') => {
                    self.pos += 1;
                    cmd.quote();
                    self.double(cmd, depth)?;
                }
                Some('(') => {
                    self.pos += 1;
                    self.list(depth + 1, true)?;
                    cmd.push(UNKNOWN);
                }
                _ => cmd.push('$'),
            },
            c => cmd.push(c),
        }
        Ok(())
    }

    /// The rest of a double-quoted string, after its `"`.
    fn double(&mut self, cmd: &mut Command, depth: usize) -> Result<(), TooDeep> {
        while let Some(c) = self.next() {
            match c {
                '"' => break,
                '\\' => match self.next_if(|c| matches!(c, '\\' | '"' | '$' | '`' | '\n')) {
                    Some('\n') => {}
                    Some(c) => cmd.word.push(c),
                    None => cmd.word.push('\\'),
                },
                '`' => {
                    self.backquote(depth, true)?;
                    cmd.word.push(UNKNOWN);
                }
                '$' if self.next_if(|c| c == '(').is_some() => {
                    self.list(depth + 1, true)?;
                    cmd.word.push(UNKNOWN);
                }
                c => cmd.word.push(c),
            }
        }
        Ok(())
    }

    /// A backquoted substitution, after its `` ` ``: its text, with the
    /// backslashes that quote `\`, `` ` `` and `$` (and `"`, inside double
    /// quotes) taken out, read as commands of its own.
    fn backquote(&mut self, depth: usize, in_double: bool) -> Result<(), TooDeep> {
        let mut text = String::new();
        while let Some(c) = self.next() {
            match c {
                '`' => break,
                '\\' => match self.next() {
                    Some(c @ ('\\' | '`' | '$')) => text.push(c),
                    Some('"') if in_double => text.push('"'),
                    Some(c) => {
                        text.push('\\');
                        text.push(c);
                    }
                    None => text.push('\\'),
                },
                c => text.push(c),
            }
        }
        let mut inner = Lexer::new(&text);
        inner.list(depth + 1, false)?;
        self.commands.append(&mut inner.commands);
        Ok(())
    }

    /// The rest of a `$'…'` string, after its `'`, its escapes decoded as
    /// bash decodes them.
    fn ansi_c(&mut self, cmd: &mut Command) {
        while let Some(c) = self.next() {
            match c {
                '\'' => break,
                '\\' => self.ansi_c_escape(cmd),
                c => cmd.word.push(c),
            }
        }
    }

    fn ansi_c_escape(&mut self, cmd: &mut Command) {
        let Some(e) = self.next() else {
            cmd.word.push('\\');
            return;
        };
        let plain = match e {
            'a' => '\x07',
            'b' => '\x08',
            'e' | 'E' => '\x1b',
            'f' => '\x0c',
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'v' => '\x0b',
            '\\' | '\'' | '"' | '?' => e,
            '0'..='7' => {
                let n = self.digits(e.to_digit(8).unwrap_or(0), 8, 2);
                cmd.word.extend(char::from_u32(n));
                return;
            }
            'x' | 'u' | 'U' => {
                let most = match e {
                    'x' => 2,
                    'u' => 4,
                    _ => 8,
                };
                if self.peek().is_some_and(|c| c.is_ascii_hexdigit()) {
                    let n = self.digits(0, 16, most);
                    cmd.word.extend(char::from_u32(n));
                } else {
                    cmd.word.push('\\');
                    cmd.word.push(e);
                }
                return;
            }
            'c' => match self.next() {
                Some(c) => char::from_u32(u32::from(c) & 0x1f).unwrap_or(c),
                None => {
                    cmd.word.push_str("\\c");
                    return;
                }
            },
            e => {
                cmd.word.push('\\');
                e
            }
        };
        cmd.word.push(plain);
    }

    /// `first` followed by up to `most` more digits in `radix`, as a number.
    fn digits(&mut self, first: u32, radix: u32, most: usize) -> u32 {
        let mut n = first;
        for _ in 0..most {
            let Some(d) = self.peek().and_then(|c| c.to_digit(radix)) else {
                break;
            };
            n = n * radix + d;
            self.pos += 1;
        }
        n
    }

    /// A redirect, after its first `<`, `>` or `&`: the rest of the
    /// operator (`>>`, `>&`, `<>`, `>|`, `<<<`), then its target, which
    /// isn't a word (`-` and a descriptor's number count as targets). A
    /// descriptor's number before it (`2>`) isn't a word either.
    fn redirect(&mut self, cmd: &mut Command) {
        if cmd.in_word && !cmd.quoted && cmd.word.bytes().all(|b| b.is_ascii_digit()) {
            cmd.word.clear();
            cmd.in_word = false;
        }
        cmd.end_word();
        while self
            .next_if(|c| matches!(c, '<' | '>' | '&' | '|'))
            .is_some()
        {}
        cmd.target = true;
    }
}

#[cfg(test)]
mod tests {
    use super::{DEPTH, TooDeep, UNKNOWN, commands};

    /// The commands, with `%` for a substitution's [`UNKNOWN`].
    fn want(cmds: &[&[&str]]) -> Result<Vec<Vec<String>>, TooDeep> {
        Ok(cmds
            .iter()
            .map(|c| {
                c.iter()
                    .map(|w| w.replace('%', &UNKNOWN.to_string()))
                    .collect()
            })
            .collect())
    }

    #[test]
    fn words_split_as_the_shell_splits_them() {
        assert_eq!(
            commands("git push -f  origin\tmain"),
            want(&[&["git", "push", "-f", "origin", "main"]])
        );
        assert_eq!(
            commands(r#"a 'b c' "d e" f\ g ''"#),
            want(&[&["a", "b c", "d e", "f g", ""]])
        );
        assert_eq!(commands(r#"a'b'"c"d"#), want(&[&["abcd"]]));
        assert_eq!(
            commands(r#""a \"b\" \$c \x""#),
            want(&[&[r#"a "b" $c \x"#]])
        );
        assert_eq!(commands(r"'a \' b"), want(&[&[r"a \", "b"]]));
        assert_eq!(commands("a\\\nb"), want(&[&["ab"]]));
        // An unclosed quote runs to the end.
        assert_eq!(commands("a 'b c"), want(&[&["a", "b c"]]));
        // Only a space, a tab or a newline splits words.
        assert_eq!(
            commands("a\u{a0}b\u{b}c\u{c}d e"),
            want(&[&["a\u{a0}b\u{b}c\u{c}d", "e"]])
        );
    }

    #[test]
    fn dollar_quotes_are_decoded() {
        assert_eq!(
            commands(r"$'git push\t-f' a$'\x6d\141i'n"),
            want(&[&["git push\t-f", "amain"]])
        );
        assert_eq!(
            commands(r#"$'\'\\\"\?\e\cA\q\x\777' $'a b"#),
            want(&[&["'\\\"?\x1b\x01\\q\\x\u{1ff}", "a b"]])
        );
        assert_eq!(commands(r#"$"a b"c"#), want(&[&["a bc"]]));
        // Not inside double quotes, and not as a plain `$`.
        assert_eq!(commands(r#""$'a'" $ $x"#), want(&[&["$'a'", "$", "$x"]]));
    }

    #[test]
    fn separators_end_a_command_unless_quoted() {
        assert_eq!(
            commands("a; b && c || d | e & f\ng"),
            want(&[&["a"], &["b"], &["c"], &["d"], &["e"], &["f"], &["g"]])
        );
        assert_eq!(
            commands("a 'b;c|d&e' \"f\ng\""),
            want(&[&["a", "b;c|d&e", "f\ng"]])
        );
        assert_eq!(commands("(a; (b)) c"), want(&[&["a"], &["b"], &["c"]]));
    }

    #[test]
    fn a_substitution_is_commands_of_its_own() {
        // The words around it stay with their command.
        assert_eq!(
            commands("a $(b c) d `e f` g"),
            want(&[&["b", "c"], &["e", "f"], &["a", "%", "d", "%", "g"]])
        );
        assert_eq!(commands("x=$(a b)y"), want(&[&["a", "b"], &["x=%y"]]));
        // Its quotes are its own, inside double quotes too.
        assert_eq!(
            commands(r#"a "x $(b "c d" e) y" f"#),
            want(&[&["b", "c d", "e"], &["a", "x % y", "f"]])
        );
        assert_eq!(
            commands(r#"a "`b \"c d\"`" e"#),
            want(&[&["b", "c d"], &["a", "%", "e"]])
        );
        // Nested, with subshells and arithmetic inside.
        assert_eq!(
            commands("a $(b $(c); (d); echo $((1+2))) e"),
            want(&[
                &["c"],
                &["b", "%"],
                &["d"],
                &["1+2"],
                &["echo", "%"],
                &["a", "%", "e"]
            ])
        );
        assert_eq!(
            commands(r"a `b \`c\` \$d`"),
            want(&[&["c"], &["b", "%", "$d"], &["a", "%"]])
        );
        // A process substitution is a word, not a redirect.
        assert_eq!(
            commands("a <(b c) >(d) e"),
            want(&[&["b", "c"], &["d"], &["a", "%", "%", "e"]])
        );
    }

    #[test]
    fn redirects_and_their_targets_arent_words() {
        assert_eq!(commands("a b > log c"), want(&[&["a", "b", "c"]]));
        assert_eq!(
            commands("a 2>/dev/null b 2>&1 c >>log <in d"),
            want(&[&["a", "b", "c", "d"]])
        );
        assert_eq!(commands("a x2>y b"), want(&[&["a", "x2", "b"]]));
        assert_eq!(commands("a '2'>y b"), want(&[&["a", "2", "b"]]));
        // `-` is the target that closes a descriptor.
        assert_eq!(commands("a 3>&- b <&- c"), want(&[&["a", "b", "c"]]));
        assert_eq!(
            commands("a &>log b &>>log c <<<x d <<EOF e"),
            want(&[&["a", "b", "c", "d", "e"]])
        );
        // What a target runs is read all the same.
        assert_eq!(
            commands(r#"a <"$(b c)" d > `e` f < <(g)"#),
            want(&[&["b", "c"], &["e"], &["g"], &["a", "d", "f"]])
        );
        // A redirect left without a target takes nothing from the next
        // command.
        assert_eq!(commands("a >; b"), want(&[&["a"], &["b"]]));
    }

    #[test]
    fn nesting_past_the_limit_is_refused() {
        assert_eq!(commands(&"$(".repeat(DEPTH + 1)), Err(TooDeep));
        assert!(commands(&"$(".repeat(DEPTH)).is_ok());
    }
}
