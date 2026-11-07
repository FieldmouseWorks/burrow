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
//! (`bash <<EOF`). An unquoted glob character or brace list marks its word
//! [`UNKNOWN`] too (a quoted one is text). Nothing else is expanded.

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
    /// Inside a `${…}`, where a glob character is the parameter's text.
    in_parameter: bool,
    /// A here-document's end word, which the shell doesn't expand: only
    /// its quotes come off.
    delimiter: bool,
    /// Each unquoted `{` open in the word, innermost last: whether a `,` or
    /// `..` of its own has made it a brace expansion.
    braces: Vec<bool>,
}

impl Command {
    fn push(&mut self, c: char) {
        self.word.push(c);
        self.in_word = true;
    }

    /// An unquoted character of a word. Where the shell would glob or brace
    /// -expand it, the word is unknown until it runs, so [`UNKNOWN`] goes in
    /// ahead of the character (`a*` reads `a￼*`, `{a,b}` reads `{a,b￼}`, and
    /// `{{},a}` reads `{{},a￼}`: an inner pair closing leaves the outer one's
    /// list open); a quoted or escaped one is plain text ([`Command::push`]).
    fn bare(&mut self, c: char) {
        if !self.in_parameter && !self.delimiter {
            match c {
                '*' | '?' | '[' => self.word.push(UNKNOWN),
                '{' => self.braces.push(false),
                // `,`, or the second `.` of a range's `..`, makes the
                // innermost `{` a list.
                ',' | '.' if c == ',' || self.word.ends_with('.') => {
                    if let Some(list) = self.braces.last_mut() {
                        *list = true;
                    }
                }
                // Every `}` closes the innermost `{`, a list or not.
                '}' if self.braces.pop() == Some(true) => self.word.push(UNKNOWN),
                _ => {}
            }
        }
        self.push(c);
    }

    fn quote(&mut self) {
        self.in_word = true;
        self.quoted = true;
    }

    fn end_word(&mut self) {
        if self.in_word {
            let word = std::mem::take(&mut self.word);
            self.braces.clear();
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

/// Each `${…}` in `word`: where it starts and, past its `}`, ends. Nested
/// ones are inside their own, for a reading of its text to find.
fn parameters(word: &str) -> Vec<(usize, usize)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(start) = word[from..].find("${").map(|at| from + at) {
        let mut depth = 0;
        let mut end = word.len();
        for (at, c) in word[start + 1..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                end = start + 1 + at + 1;
                break;
            }
        }
        found.push((start, end));
        from = end;
    }
    found
}

/// What a `${…}` holds after its parameter and operator, which may be
/// what it expands to; the operators that don't use it are read too, since
/// `${x/a/b}` can.
fn parameter_text(parameter: &str) -> Option<&str> {
    let body = parameter.strip_prefix("${")?;
    let body = body.strip_suffix('}').unwrap_or(body);
    let body = body
        .strip_prefix(['!', '#'])
        .filter(|b| !b.is_empty())
        .unwrap_or(body);
    let name = body
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(body.len());
    let name = if name == 0 && !body.is_empty() {
        body.chars().next().map_or(0, char::len_utf8)
    } else {
        name
    };
    let mut rest = &body[name..];
    if rest.starts_with('[') {
        rest = rest.split_once(']').map_or("", |(_, after)| after);
    }
    const OPERATORS: [&str; 22] = [
        ":-", ":=", ":+", ":?", "##", "%%", "//", "/#", "/%", "^^", ",,", "-", "=", "+", "?", "#",
        "%", "/", "^", ",", ":", "@",
    ];
    let text = OPERATORS
        .iter()
        .find_map(|op| rest.strip_prefix(op))
        .unwrap_or(rest);
    (!text.is_empty()).then_some(text)
}

/// The text of each `${…}` in `word` that holds any.
pub fn parameter_texts(word: &str) -> Vec<&str> {
    parameters(word)
        .into_iter()
        .filter_map(|(start, end)| parameter_text(&word[start..end]))
        .collect()
}

/// Whether `word` is one `${…}` with a text, and nothing else.
pub fn is_only_a_parameter_text(word: &str) -> bool {
    matches!(parameters(word).as_slice(), [(0, end)] if *end == word.len())
        && parameter_text(word).is_some()
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
                    let outer = std::mem::replace(&mut cmd.in_parameter, true);
                    let read = self.parameter(cmd, depth + 1);
                    cmd.in_parameter = outer;
                    read?;
                }
                _ => cmd.push('$'),
            },
            c => cmd.bare(c),
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

    /// A here-document's end word, after its `<<`: quotes come off it, and
    /// nothing in it is expanded, so a glob, a brace list, a `${…}` or a
    /// substitution is its own text (`<<E?` ends at `E?`, `<<$(x)` at
    /// `$(x)`). A substitution's commands are still read.
    fn heredoc(&mut self, depth: usize) -> Result<(), TooDeep> {
        let strip_tabs = self.next_if(|c| c == '-').is_some();
        while self.next_if(|c| matches!(c, ' ' | '\t')).is_some() {}
        let mut end = Command {
            delimiter: true,
            ..Command::default()
        };
        while let Some(c) = self.next_if(|c| {
            !matches!(
                c,
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>'
            )
        }) {
            let (start, before) = (self.pos - 1, end.word.len());
            self.word(c, &mut end, depth)?;
            let quoted = c == '$' && matches!(self.chars.get(start + 1), Some('\'' | '"'));
            if matches!(c, '$' | '`') && !quoted {
                end.word.truncate(before);
                end.word.extend(&self.chars[start..self.pos]);
            }
        }
        self.heredocs.push(Heredoc {
            end: end.word,
            strip_tabs,
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests;
