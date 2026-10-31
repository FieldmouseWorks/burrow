//! Each rule's own test of what it reads: a title, a body, a path, a file's
//! first line or a line a PR adds.

use std::sync::LazyLock;

use regex::Regex;

/// A rule a PR is held to, each a commit status of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rule {
    /// The title is `area: plain sentence`, the house commit style: the
    /// steward squash-merges, so the title is the subject main gets.
    Title,
    /// Code starts in Rust: a PR that adds Python, or grows Python and shell
    /// past a fix, says why not.
    RustFirst,
    /// Work starts from events: a PR that adds a timer says why.
    Timers,
    /// A PR that grows a code file past the limit, or one already past it,
    /// splits it or says why not.
    FileSize,
    /// Tracked files never name a model: a PR that adds a line naming one
    /// says why.
    ModelNames,
}

impl Rule {
    pub const ALL: [Rule; 5] = [
        Rule::Title,
        Rule::RustFirst,
        Rule::Timers,
        Rule::FileSize,
        Rule::ModelNames,
    ];

    /// Its name in its status's context, `house/NAME (#N)`.
    pub fn name(self) -> &'static str {
        match self {
            Rule::Title => "title",
            Rule::RustFirst => "rust-first",
            Rule::Timers => "timers",
            Rule::FileSize => "file-size",
            Rule::ModelNames => "model-names",
        }
    }

    /// The line a PR's body says why on, for a rule that takes one.
    pub fn line(self) -> Option<&'static str> {
        match self {
            Rule::Title => None,
            Rule::RustFirst => Some("Why not Rust"),
            Rule::Timers => Some("Why a timer"),
            Rule::FileSize => Some("Why not split"),
            Rule::ModelNames => Some("Why a model name"),
        }
    }
}

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a rule's pattern compiles")
}

/// Why a title isn't `area: plain sentence`, or nothing. An area is up to
/// three words naming a module, directory or tool, several joined by commas;
/// a commit type isn't one. Capitals aren't judged: across main's subjects,
/// every capitalized first word was a proper noun. `rebase!` is Renovate's
/// mark, which its sweep puts on a title and takes off again.
pub fn title_style(title: &str) -> Option<&'static str> {
    static AREAS: LazyLock<Regex> = LazyLock::new(|| {
        let word = "[A-Za-z0-9._][A-Za-z0-9._/@+-]*";
        let area = format!("{word}(?: {word}){{0,2}}");
        re(&format!("^({area}(?:, {area})*): (.*)$"))
    });
    let title = title.strip_prefix("rebase!").unwrap_or(title);
    let Some(parts) = AREAS.captures(title) else {
        return Some("it doesn't start with an area and a colon");
    };
    let types = [
        "feat", "fix", "chore", "refactor", "style", "perf", "revert", "wip",
    ];
    if parts[1]
        .split(", ")
        .any(|area| types.contains(&area.to_lowercase().as_str()))
    {
        return Some("its area is a commit type");
    }
    let sentence = &parts[2];
    if sentence.is_empty() || sentence.starts_with(char::is_whitespace) {
        Some("there's no sentence after the colon")
    } else if sentence.ends_with('.') {
        Some("it ends with a period")
    } else {
        None
    }
}

/// How few words a reason may have: a reason says why in a sentence, and
/// "n/a", "one-off" or "coin toss" isn't one.
pub const REASON_WORDS: usize = 4;

/// What a body says on a rule's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Said {
    /// No line starts with it.
    Nothing,
    /// It has the line, but no reason after it: fewer than REASON_WORDS
    /// words.
    TooLittle,
    /// The line, with a reason.
    Why,
}

/// What the body says on a line starting `LINE:`, in any case, bold or in a
/// list or a quote.
pub fn says(body: &str, line: &str) -> Said {
    let start = re(&format!(
        r"(?i)^[\s>*_-]*{}[*_]*:(.*)$",
        regex::escape(line)
    ));
    let mut said = Said::Nothing;
    for l in body.lines() {
        let Some(parts) = start.captures(l) else {
            continue;
        };
        let words = parts[1]
            .split_whitespace()
            .filter(|w| w.chars().any(char::is_alphanumeric))
            .count();
        if words >= REASON_WORDS {
            return Said::Why;
        }
        said = Said::TooLittle;
    }
    said
}

/// The scripting languages the house rules want ported to Rust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    Python,
    Shell,
}

/// A path's language by its name, when its name says: by its extension, and
/// a name with none (`bin/tool`) only says by its first line.
pub fn by_name(path: &str) -> Option<Option<Language>> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let (_, extension) = name.rsplit_once('.')?;
    Some(match extension {
        "py" | "pyi" => Some(Language::Python),
        "sh" | "bash" | "zsh" => Some(Language::Shell),
        _ => None,
    })
}

/// A file's language by the shebang on its first line.
pub fn by_shebang(content: &[u8]) -> Option<Language> {
    static PYTHON: LazyLock<Regex> = LazyLock::new(|| re("^#!.*python"));
    static SHELL: LazyLock<Regex> = LazyLock::new(|| re(r"^#!.*\b(sh|bash|zsh|dash|ksh)\b"));
    let first = content.split(|b| *b == b'\n').next().unwrap_or_default();
    let first = String::from_utf8_lossy(first);
    if PYTHON.is_match(&first) {
        Some(Language::Python)
    } else if SHELL.is_match(&first) {
        Some(Language::Shell)
    } else {
        None
    }
}

/// Whether a path is code someone writes: Python or shell, or code by its
/// extension, and never what a tool generates.
pub fn is_code(path: &str, language: Option<Language>) -> bool {
    let generated = format!("/{path}").contains("/generated/")
        || path.contains(".generated.")
        || path.contains(".gen.")
        || path.ends_with(".min.js")
        || path.ends_with("_pb2.py");
    if generated {
        return false;
    }
    if language.is_some() {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        matches!(
            extension,
            "rs" | "py"
                | "pyi"
                | "sh"
                | "bash"
                | "zsh"
                | "nix"
                | "ts"
                | "tsx"
                | "mts"
                | "cts"
                | "js"
                | "jsx"
                | "mjs"
                | "cjs"
                | "svelte"
                | "vue"
                | "go"
                | "java"
                | "kt"
                | "kts"
                | "gd"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "hpp"
                | "cs"
                | "rb"
                | "lua"
                | "swift"
        )
    })
}

/// Whether a line sets up a timer: a systemd timer's keys, `startAt`, a
/// systemd timer unit in Nix, a cron schedule.
pub fn timer(line: &str) -> bool {
    static TIMER: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"\b(OnCalendar|OnActiveSec|OnBootSec|OnStartupSec|OnUnitActiveSec|OnUnitInactiveSec|timerConfig|systemCronJobs)\b",
            r"|\bstartAt\s*=",
            r"|\bsystemd\.(user\.)?timers\b",
            r"|^\s*-?\s*cron\s*:",
        ))
    });
    TIMER.is_match(line)
}

/// Whether a line is a comment, in any of the house's languages.
pub fn comment(line: &str) -> bool {
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| re(r"^\s*(#|//|--|;|/\*|\*)"));
    COMMENT.is_match(line)
}

/// Whether a line names a model. Case matters, so a word that only looks
/// like one (an opus, sonnet 18) passes.
pub fn model(line: &str) -> bool {
    static MODEL: LazyLock<Regex> = LazyLock::new(|| {
        re(concat!(
            r"\bclaude-(opus|sonnet|haiku|fable|instant|[0-9])",
            r"|\b(Opus|Sonnet|Haiku|Fable) [0-9]",
            r"|\b[Gg][Pp][Tt]-?[0-9]",
            r"|\b[Gg]emini[- ][0-9]",
            r"|\b[Ll]lama[- ]?[0-9]",
            r"|\b[Qq]wen[- ]?[0-9]",
            r"|\bDeepSeek-[VR][0-9]",
            r"|\bo[134]-(mini|pro)\b",
        ))
    });
    MODEL.is_match(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages() {
        assert_eq!(by_name("tools/a.py"), Some(Some(Language::Python)));
        assert_eq!(by_name("a/b.pyi"), Some(Some(Language::Python)));
        assert_eq!(by_name("x/run.sh"), Some(Some(Language::Shell)));
        assert_eq!(by_name("x/run.bash"), Some(Some(Language::Shell)));
        assert_eq!(by_name("x/rc.zsh"), Some(Some(Language::Shell)));
        assert_eq!(by_name("src/main.rs"), Some(None));
        assert_eq!(by_name("v1.2/README"), None);
        assert_eq!(by_name("bin/tool"), None);
        for (first, language) in [
            ("#!/usr/bin/env python3", Some(Language::Python)),
            ("#! /usr/bin/python", Some(Language::Python)),
            ("#!/bin/sh", Some(Language::Shell)),
            ("#!/usr/bin/env bash", Some(Language::Shell)),
            ("#!/usr/bin/env -S bash -eu", Some(Language::Shell)),
            ("#!/nix/store/x-bash-5.2/bin/bash", Some(Language::Shell)),
            ("#!/bin/dash", Some(Language::Shell)),
            ("#!/usr/bin/env zsh", Some(Language::Shell)),
            ("#!/usr/bin/env node", None),
            ("#!/usr/bin/env nix-shell", None),
            ("echo #!/bin/sh", None),
            ("", None),
        ] {
            let content = format!("{first}\nmore\n");
            assert_eq!(by_shebang(content.as_bytes()), language, "{first}");
        }
        assert_eq!(by_shebang(b"\xff\xfe#!/bin/sh"), None);
    }

    #[test]
    fn reasons() {
        assert_eq!(
            says(
                "x\nWhy not Rust: a one-off migration script\n",
                "Why not Rust"
            ),
            Said::Why
        );
        assert_eq!(
            says("Why not Rust: one-off", "Why not Rust"),
            Said::TooLittle
        );
        assert_eq!(says("Why not Rust: n/a", "Why not Rust"), Said::TooLittle);
        assert_eq!(
            says("Why not Rust: coin toss", "Why not Rust"),
            Said::TooLittle
        );
        // Words are words: marks and dashes between them aren't.
        assert_eq!(
            says("Why not Rust: - * a b c", "Why not Rust"),
            Said::TooLittle
        );
        // One line with a reason is enough, wherever it is.
        assert_eq!(
            says(
                "Why not Rust: n/a\nWhy not Rust: Gradle generates this file",
                "Why not Rust"
            ),
            Said::Why
        );
        assert_eq!(
            says("Why not split: one table", "Why not Rust"),
            Said::Nothing
        );
    }
}
