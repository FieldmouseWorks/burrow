//! A command line read as the shell reads it, far enough to know what each
//! command it runs is given. Quotes (`'…'`, `"…"`, `$'…'`, `$"…"`) and
//! backslashes join and escape, and an unquoted `;`, `&`, `|`, `(`, `)` or
//! newline ends a command. A redirect and its target aren't words. A
//! substitution (`$(…)`, `` `…` ``, `<(…)`, `>(…)`) is read as commands of
//! its own wherever it is, inside double quotes or a redirect's target too,
//! and stands in its word as [`UNKNOWN`]. Enough of the grammar is followed
//! to find where a substitution ends: a `)` can close a subshell or a `case`
//! pattern first, and a comment, a here-document or a `${…}` holds text that
//! isn't the shell's to read as quotes or parentheses. What a comment or a
//! here-document says is still read, line by line, since it may be a script
//! (`bash <<EOF`). Nothing else is expanded.

use crate::ansi_c;

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
    /// Here-documents whose bodies start at the next newline.
    heredocs: Vec<Heredoc>,
}

struct Heredoc {
    /// The line that ends it.
    end: String,
    /// `<<-`: tabs before that line don't count.
    strip_tabs: bool,
}

/// What a scope of the reading is inside, innermost last.
enum Open {
    Subshell,
    /// `case`, up to its `in`.
    CaseHead {
        subject: bool,
    },
    /// A `case`'s patterns, up to the `)` that ends them.
    CasePatterns {
        /// The next word, if `esac`, ends the `case`: nothing of a pattern
        /// has been read, not even its optional `(`.
        may_end: bool,
        /// Extended globs open in the pattern (`@(a|b)`).
        globs: usize,
    },
    /// The commands of a `case`'s pattern, up to `;;` or `esac`.
    CaseBody,
}

/// The grammar one scope needs to tell what a `)` closes.
#[derive(Default)]
struct Grammar {
    open: Vec<Open>,
    /// A command's name has been read: the words after it aren't keywords.
    named: bool,
}

/// Words a command may follow without having begun: `then case …`.
const BEFORE_A_COMMAND: [&str; 13] = [
    "if", "then", "elif", "else", "do", "while", "until", "!", "{", "time", "coproc", "-p", "--",
];

impl Grammar {
    /// The word `cmd` holds, about to end.
    fn word(&mut self, cmd: &Command) {
        if !cmd.in_word || cmd.target {
            return;
        }
        let word = if cmd.quoted { "" } else { cmd.word.as_str() };
        match self.open.last_mut() {
            Some(Open::CaseHead { subject: false }) => {
                self.open.pop();
                self.open.push(Open::CaseHead { subject: true });
            }
            Some(Open::CaseHead { subject: true }) => {
                if word == "in" {
                    self.open.pop();
                    self.open.push(Open::CasePatterns {
                        may_end: true,
                        globs: 0,
                    });
                }
            }
            Some(Open::CasePatterns { may_end, .. }) => {
                if *may_end && word == "esac" {
                    self.open.pop();
                    self.named = true;
                } else {
                    *may_end = false;
                }
            }
            // `function NAME {`: `function` names nothing, but its body's
            // `{` comes after its name.
            _ if word == "{" && cmd.words.first().is_some_and(|w| w == "function") => {
                self.named = false;
            }
            _ if self.named => {}
            last => {
                if word == "case" {
                    self.open.push(Open::CaseHead { subject: false });
                    self.named = true;
                } else if word == "esac" && matches!(last, Some(Open::CaseBody)) {
                    self.open.pop();
                    self.named = true;
                } else {
                    self.named = !BEFORE_A_COMMAND.contains(&word);
                }
            }
        }
    }

    /// A `(`; `in_word` if it comes straight after part of a word.
    fn opens(&mut self, in_word: bool) {
        self.named = false;
        match self.open.last_mut() {
            Some(Open::CasePatterns { may_end, globs }) => {
                if in_word || !*may_end {
                    *globs += 1;
                }
                *may_end = false;
            }
            _ => self.open.push(Open::Subshell),
        }
    }

    /// A `)`: whether nothing here holds it, so it's the scope's own.
    fn closes(&mut self) -> bool {
        self.named = false;
        match self.open.last_mut() {
            Some(Open::Subshell) => {
                self.open.pop();
                false
            }
            Some(Open::CasePatterns { globs, .. }) if *globs > 0 => {
                *globs -= 1;
                false
            }
            Some(Open::CasePatterns { .. }) => {
                self.open.pop();
                self.open.push(Open::CaseBody);
                false
            }
            _ => true,
        }
    }

    /// A `|`, `;`, `&` or newline.
    fn separator(&mut self, c: char) {
        self.named = false;
        if c == '|'
            && let Some(Open::CasePatterns { may_end, .. }) = self.open.last_mut()
        {
            *may_end = false;
        }
    }

    fn in_body(&self) -> bool {
        matches!(self.open.last(), Some(Open::CaseBody))
    }

    /// A `;;`, `;&` or `;;&`: the next pattern.
    fn next_pattern(&mut self) {
        self.named = false;
        self.open.pop();
        self.open.push(Open::CasePatterns {
            may_end: true,
            globs: 0,
        });
    }
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
    /// That target is a here-string's text.
    here_string: bool,
    /// Here-strings' texts: a shell given one runs it.
    scripts: Vec<String>,
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
            } else if std::mem::take(&mut self.here_string) {
                self.scripts.push(word);
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
        self.here_string = false;
        if !self.words.is_empty() {
            commands.push(std::mem::take(&mut self.words));
        }
        commands.extend(self.scripts.drain(..).map(|script| vec![script]));
    }
}

impl Lexer {
    fn new(text: &str) -> Lexer {
        Lexer {
            chars: text.chars().collect(),
            pos: 0,
            commands: Vec::new(),
            heredocs: Vec::new(),
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
        let mut grammar = Grammar::default();
        while let Some(c) = self.next() {
            let in_word = cmd.in_word;
            let process = matches!(c, '<' | '>') && self.peek() == Some('(');
            if matches!(
                c,
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
            ) && !process
            {
                grammar.word(&cmd);
            }
            match c {
                ' ' | '\t' | '\r' => cmd.end_word(),
                '(' => {
                    grammar.opens(in_word);
                    cmd.end(&mut self.commands);
                }
                ')' => {
                    if grammar.closes() && closes {
                        break;
                    }
                    cmd.end(&mut self.commands);
                }
                '#' if !in_word => self.comment(depth)?,
                // `&>` and `&>>` send both outputs: a redirect, not the end.
                '&' if self.peek() == Some('>') => self.redirect(c, &mut cmd, depth)?,
                ';' if grammar.in_body() && matches!(self.peek(), Some(';' | '&')) => {
                    self.pos += 1;
                    if self.chars.get(self.pos) == Some(&'&') && self.chars[self.pos - 1] == ';' {
                        self.pos += 1;
                    }
                    grammar.next_pattern();
                    cmd.end(&mut self.commands);
                }
                '\n' => {
                    grammar.separator(c);
                    cmd.end(&mut self.commands);
                    self.heredoc_bodies(depth, closes)?;
                }
                ';' | '&' | '|' => {
                    grammar.separator(c);
                    cmd.end(&mut self.commands);
                }
                '<' | '>' if process => {
                    self.pos += 1;
                    self.list(depth + 1, true)?;
                    cmd.push(UNKNOWN);
                }
                '<' | '>' => self.redirect(c, &mut cmd, depth)?,
                c => self.word(c, &mut cmd, depth)?,
            }
        }
        cmd.end(&mut self.commands);
        Ok(())
    }

    /// `text` read as a script of its own, its commands joining these.
    fn read_apart(&mut self, text: &str, depth: usize) -> Result<(), TooDeep> {
        let mut inner = Lexer::new(text);
        inner.list(depth + 1, false)?;
        self.commands.append(&mut inner.commands);
        Ok(())
    }

    /// The rest of the line, after its `#`.
    fn comment(&mut self, depth: usize) -> Result<(), TooDeep> {
        let mut text = String::new();
        while let Some(c) = self.next_if(|c| c != '\n') {
            text.push(c);
        }
        self.read_apart(&text, depth)
    }

    /// The bodies of the here-documents waiting on this newline, each line
    /// read on its own: a quote in one doesn't reach the next. In a
    /// substitution (`closes`), the end word with the substitution's `)`
    /// straight after it ends the body too, as it does in bash.
    fn heredoc_bodies(&mut self, depth: usize, closes: bool) -> Result<(), TooDeep> {
        for doc in std::mem::take(&mut self.heredocs) {
            while self.peek().is_some() {
                let start = self.pos;
                let mut line = String::new();
                while let Some(c) = self.next_if(|c| c != '\n') {
                    line.push(c);
                }
                let last = if doc.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line.as_str()
                };
                if closes
                    && last
                        .strip_prefix(doc.end.as_str())
                        .is_some_and(|rest| rest.starts_with(')'))
                {
                    // Back to the `)`, for the substitution to end at.
                    self.pos = start + (line.len() - last.len()) + doc.end.chars().count();
                    break;
                }
                self.pos += 1;
                if last == doc.end {
                    break;
                }
                self.read_apart(&line, depth)?;
            }
        }
        Ok(())
    }

    /// A character of a word, outside any quotes.
    fn word(&mut self, c: char, cmd: &mut Command, depth: usize) -> Result<(), TooDeep> {
        match c {
            '\\' => match self.next() {
                Some('\n') | None => {}
                Some(c) => {
                    cmd.quote();
                    cmd.push(c);
                }
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
                Some('{') => {
                    self.pos += 1;
                    cmd.word.push_str("${");
                    self.parameter(cmd, depth + 1)?;
                }
                _ => cmd.push('$'),
            },
            c => cmd.push(c),
        }
        Ok(())
    }

    /// The rest of a `${…}`, after its `{`: part of one word, spaces and
    /// parentheses and all, with its quotes and substitutions read as usual.
    fn parameter(&mut self, cmd: &mut Command, depth: usize) -> Result<(), TooDeep> {
        if depth > DEPTH {
            return Err(TooDeep);
        }
        while let Some(c) = self.next() {
            if c == '}' {
                cmd.word.push(c);
                break;
            }
            self.word(c, cmd, depth)?;
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
        self.read_apart(&text, depth)
    }

    /// The rest of a `$'…'` string, after its `'`. Its end is found first,
    /// a backslash taking the character after it, then its escapes decoded.
    fn ansi_c(&mut self, cmd: &mut Command) {
        let mut raw = Vec::new();
        while let Some(c) = self.next() {
            match c {
                '\'' => break,
                '\\' => {
                    raw.push(c);
                    raw.extend(self.next());
                }
                c => raw.push(c),
            }
        }
        ansi_c::decode(&raw, &mut cmd.word);
    }

    /// A redirect, after its first `<`, `>` or `&` (`first`): the rest of
    /// the operator (`>>`, `>&`, `<>`, `>|`, `<<<`), then its target, which
    /// isn't a word (`-` and a descriptor's number count as targets). A
    /// descriptor's number before it (`2>`) isn't a word either. `<<` and
    /// `<<-` take a here-document's end word for a target.
    fn redirect(&mut self, first: char, cmd: &mut Command, depth: usize) -> Result<(), TooDeep> {
        if cmd.in_word && !cmd.quoted && cmd.word.bytes().all(|b| b.is_ascii_digit()) {
            cmd.word.clear();
            cmd.in_word = false;
        }
        cmd.end_word();
        let mut operator = String::from(first);
        while let Some(c) = self.next_if(|c| matches!(c, '<' | '>' | '&' | '|')) {
            operator.push(c);
        }
        if operator == "<<" {
            return self.heredoc(depth);
        }
        cmd.target = true;
        cmd.here_string = operator == "<<<";
        Ok(())
    }

    /// A here-document's end word, after its `<<`: quotes come off it.
    fn heredoc(&mut self, depth: usize) -> Result<(), TooDeep> {
        let strip_tabs = self.next_if(|c| c == '-').is_some();
        while self.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        let mut end = Command::default();
        while let Some(c) = self.next_if(|c| {
            !matches!(
                c,
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
            )
        }) {
            self.word(c, &mut end, depth)?;
        }
        self.heredocs.push(Heredoc {
            end: end.word,
            strip_tabs,
        });
        Ok(())
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
            want(&[&["'\\\"?\x1b\x01\\q\\x\u{fffd}", "a b"]])
        );
        assert_eq!(commands(r#"$"a b"c"#), want(&[&["a bc"]]));
        // A byte, and a NUL ending only its own string.
        assert_eq!(commands(r"$'git\440push'"), want(&[&["git push"]]));
        assert_eq!(commands(r"a$'b\0c'd e"), want(&[&["abd", "e"]]));
        // A backslash takes the quote after it, whatever it escapes.
        assert_eq!(commands(r"$'\c' b"), want(&[&[r"\c", "b"]]));
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
            want(&[&["a", "b", "c", "d", "e"], &["x"]])
        );
        // What a target runs is read all the same.
        assert_eq!(
            commands(r#"a <"$(b c)" d > `e` f < <(g)"#),
            want(&[&["b", "c"], &["e"], &["g"], &["a", "d", "f"]])
        );
        // A here-string's text is no word of its command, and stands apart.
        assert_eq!(
            commands("a <<< 'b c' d; e"),
            want(&[&["a", "d"], &["b c"], &["e"]])
        );
        assert_eq!(commands("<<<a"), want(&[&["a"]]));
        // A redirect left without a target takes nothing from the next
        // command.
        assert_eq!(commands("a >; b"), want(&[&["a"], &["b"]]));
    }

    #[test]
    fn a_case_pattern_doesnt_end_a_substitution() {
        let inner = ["case", "x", "in", "x"];
        for (text, words) in [
            ("a $(case x in x) b ;; esac) c", vec!["a", "%", "c"]),
            // Its optional `(`, its alternatives, and every way to the next.
            ("a $(case x in (x) b ;; esac) c", vec!["a", "%", "c"]),
            ("a $(case x in x|y) b ;; esac) c", vec!["a", "%", "c"]),
            ("a $(case x in x) b ;& esac) c", vec!["a", "%", "c"]),
            ("a $(case x in x) b ;;& esac) c", vec!["a", "%", "c"]),
            ("a $(case x in x) b; esac) c", vec!["a", "%", "c"]),
            ("a $(case x in\nx)\nb\nesac\n) c", vec!["a", "%", "c"]),
            ("a <(case x in x) b ;; esac) c", vec!["a", "%", "c"]),
        ] {
            let read = commands(text).unwrap();
            let last: Vec<_> = read
                .last()
                .unwrap()
                .iter()
                .map(|w| w.replace(UNKNOWN, "%"))
                .collect();
            assert_eq!(last, words, "{text}");
            assert!(read.contains(&vec!["b".to_owned()]), "{text}: {read:?}");
            assert_eq!(read[0][..2], inner[..2], "{text}");
        }
        // A second pattern after the first's body, and `esac` as a pattern.
        for text in [
            "a $(case x in y) d ;; z) b ;; esac) c",
            "a $(case x in y|esac) d ;; (esac) b ;; esac) c",
            "a $(case x in @(y|z)) b ;; esac) c",
            "a $(case x in z) (d) ; b ;; esac) c",
            "a $(if t; then case x in z) b ;; esac; fi) c",
            "a $(function f { case x in z) b ;; esac; }) c",
            "a $(case x in z) case y in w) b ;; esac ;; esac) c",
            "a $(case x in esac; b) c",
            "a $(case ')' in ')') b ;; esac) c",
        ] {
            let read = commands(text).unwrap();
            assert_eq!(read.last().unwrap().len(), 3, "{text}: {read:?}");
            assert!(read.contains(&vec!["b".to_owned()]), "{text}: {read:?}");
        }
        // Not the keyword: a quoted or an argument `case` opens nothing.
        assert_eq!(
            commands("a $('case' x in x) b"),
            want(&[&["case", "x", "in", "x"], &["a", "%", "b"]])
        );
        assert_eq!(
            commands("a $(echo case x in x) b"),
            want(&[&["echo", "case", "x", "in", "x"], &["a", "%", "b"]])
        );
    }

    #[test]
    fn a_comment_holds_no_quotes_or_parentheses() {
        // `)` and `'` in a comment are its own; what it says is read too.
        assert_eq!(
            commands("a $(b # c) d\ne) f"),
            want(&[&["c"], &["d"], &["b"], &["e"], &["a", "%", "f"]])
        );
        assert_eq!(
            commands("a # don't\nb c"),
            want(&[&["dont"], &["a"], &["b", "c"]])
        );
        // Only at the start of a word.
        assert_eq!(commands("a b#c $#"), want(&[&["a", "b#c", "$#"]]));
    }

    #[test]
    fn a_here_document_holds_no_quotes_or_parentheses() {
        assert_eq!(
            commands("a <<EOF\ndon't\n)\nEOF\nb c"),
            want(&[&["a"], &["dont"], &["b", "c"]])
        );
        // Closing a substitution only after its body, or with its end
        // word, as bash does.
        assert_eq!(
            commands("a $(b <<EOF\n)\nEOF\nc) d"),
            want(&[&["b"], &["c"], &["a", "%", "d"]])
        );
        assert_eq!(
            commands("a \"$(b <<EOF\nc\nEOF)\" d $(e <<-EOF\n\tEOF) f"),
            want(&[&["b"], &["c"], &["e"], &["a", "%", "d", "%", "f"]])
        );
        // Its body is read, its end word's quotes come off, `<<-` strips tabs.
        assert_eq!(
            commands("bash <<'E F'\ngit push\nE F\nb"),
            want(&[&["bash"], &["git", "push"], &["b"]])
        );
        assert_eq!(
            commands("a <<-EOF c\n\tb\n\tEOF\nd"),
            want(&[&["a", "c"], &["b"], &["d"]])
        );
        // Each body in turn, and one that never ends runs out with the text.
        assert_eq!(
            commands("a <<X <<Y\nb\nX\nc\nY\nd"),
            want(&[&["a"], &["b"], &["c"], &["d"]])
        );
        assert_eq!(commands("a <<X\nb"), want(&[&["a"], &["b"]]));
    }

    #[test]
    fn a_parameter_holds_its_parentheses() {
        assert_eq!(
            commands("a $(b ${x:-)} c) d"),
            want(&[&["b", "${x:-)}", "c"], &["a", "%", "d"]])
        );
        assert_eq!(commands("${x:-$(a b)}"), want(&[&["a", "b"], &["${x:-%}"]]));
    }

    #[test]
    fn nesting_past_the_limit_is_refused() {
        assert_eq!(commands(&"$(".repeat(DEPTH + 1)), Err(TooDeep));
        assert!(commands(&"$(".repeat(DEPTH)).is_ok());
        assert_eq!(commands(&"${".repeat(DEPTH + 1)), Err(TooDeep));
    }
}
