//! A command line split into commands and words as the shell splits it, far
//! enough to read what a command is given: quotes and backslashes join and
//! escape, and an unquoted `;`, `&`, `|`, `(`, `)`, backtick or newline ends
//! a command. A redirect and its target aren't words. Nothing is expanded.

/// Each command in `text`, as its words.
pub fn commands(text: &str) -> Vec<Vec<String>> {
    let mut lex = Lexer::default();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' => lex.end_word(),
            '\n' | ';' | '&' | '|' | '(' | ')' | '`' => lex.end_command(),
            '<' | '>' => {
                // `2>`: the descriptor isn't a word either.
                if lex.in_word && !lex.quoted && lex.word.bytes().all(|b| b.is_ascii_digit()) {
                    lex.word.clear();
                    lex.in_word = false;
                }
                lex.end_word();
                // The rest of the operator (`>>`, `>&`, `<>`, `>|`); its
                // target follows, `-` and a descriptor's number included.
                while chars
                    .next_if(|c| matches!(c, '<' | '>' | '&' | '|'))
                    .is_some()
                {}
                lex.target = true;
            }
            '\\' => match chars.next() {
                Some('\n') | None => {}
                Some(c) => lex.push(c),
            },
            '\'' => {
                lex.start_quote();
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    lex.word.push(c);
                }
            }
            '"' => {
                lex.start_quote();
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => match chars.next_if(|c| matches!(c, '\\' | '"' | '$' | '`' | '\n'))
                        {
                            Some('\n') => {}
                            Some(c) => lex.word.push(c),
                            None => lex.word.push('\\'),
                        },
                        c => lex.word.push(c),
                    }
                }
            }
            c => lex.push(c),
        }
    }
    lex.end_command();
    lex.commands
}

#[derive(Default)]
struct Lexer {
    commands: Vec<Vec<String>>,
    words: Vec<String>,
    word: String,
    /// A word has begun, even an empty one (`''`).
    in_word: bool,
    /// Some of the word was quoted or escaped.
    quoted: bool,
    /// The next word is a redirect's target.
    target: bool,
}

impl Lexer {
    fn push(&mut self, c: char) {
        self.word.push(c);
        self.in_word = true;
    }

    fn start_quote(&mut self) {
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

    fn end_command(&mut self) {
        self.end_word();
        // `<(` is a command, not the redirect's target.
        self.target = false;
        if !self.words.is_empty() {
            self.commands.push(std::mem::take(&mut self.words));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::commands;

    fn want(cmds: &[&[&str]]) -> Vec<Vec<String>> {
        cmds.iter()
            .map(|c| c.iter().map(|w| (*w).to_owned()).collect())
            .collect()
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
    }

    #[test]
    fn separators_end_a_command_unless_quoted() {
        assert_eq!(
            commands("a; b && c || d | e & f\ng"),
            want(&[&["a"], &["b"], &["c"], &["d"], &["e"], &["f"], &["g"]])
        );
        assert_eq!(
            commands("x=$(a b) `c d`"),
            want(&[&["x=$"], &["a", "b"], &["c", "d"]])
        );
        assert_eq!(
            commands("a 'b;c|d&e' \"f\ng\""),
            want(&[&["a", "b;c|d&e", "f\ng"]])
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
        // A process substitution is a command of its own, not a target.
        assert_eq!(
            commands("a <(b c) >(d)"),
            want(&[&["a"], &["b", "c"], &["d"]])
        );
    }
}
