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
    /// A PR that grows a non-test code file past the limit, or one already
    /// past it, splits it or says why not.
    FileSize,
    /// An AGENTS.md is a map: a PR that grows one's own part past GUIDE
    /// lines says why.
    GuideSize,
    /// Tracked files never name a model: a PR that adds a line naming one
    /// says why.
    ModelNames,
    /// The head isn't under Renovate's branch prefix unless Renovate opened
    /// the PR: Renovate takes every branch there as its own.
    Branch,
    /// The title and body never write the review connector's mention: the
    /// connector reads one that does as a task, and doesn't review the head.
    Mention,
}

impl Rule {
    pub const ALL: [Rule; 8] = [
        Rule::Title,
        Rule::RustFirst,
        Rule::Timers,
        Rule::FileSize,
        Rule::GuideSize,
        Rule::ModelNames,
        Rule::Branch,
        Rule::Mention,
    ];

    /// Its name in its status's context, `house/NAME (#N)`.
    pub fn name(self) -> &'static str {
        match self {
            Rule::Title => "title",
            Rule::RustFirst => "rust-first",
            Rule::Timers => "timers",
            Rule::FileSize => "file-size",
            Rule::GuideSize => "guide-size",
            Rule::ModelNames => "model-names",
            Rule::Branch => "branch",
            Rule::Mention => "mention",
        }
    }

    /// The line a PR's body says why on, for a rule that takes one.
    pub fn line(self) -> Option<&'static str> {
        match self {
            Rule::Title | Rule::Branch | Rule::Mention => None,
            Rule::RustFirst => Some("Why not Rust"),
            Rule::Timers => Some("Why a timer"),
            Rule::FileSize => Some("Why not split"),
            Rule::GuideSize => Some("Why a longer guide"),
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

/// Renovate's branch prefix, its `branchPrefix`, which the machine's
/// policy (modules/updates/renovate-policy.nix) leaves at Renovate's
/// default. Renovate takes every branch under it for its own: one it has no
/// update for it calls abandoned, renaming its PR's title `... - abandoned`,
/// a title the steward would squash onto main.
pub const RENOVATE_PREFIX: &str = "renovate/";

/// The mark every PR body Renovate writes ends with, on a line of its own:
/// a PR's author can't tell Renovate's PR from a job's, as both act on
/// GitHub as the same App.
pub const RENOVATE_MARK: &str = "<!--renovate-debug:";

/// Whether a PR whose head is the branch `head`, in the PR's own repo, and
/// whose body is `body` sits on a branch Renovate takes for its own without
/// being Renovate's PR.
pub fn renovate_claims(head: &str, body: &str) -> bool {
    head.starts_with(RENOVATE_PREFIX) && !body.lines().any(|l| l.starts_with(RENOVATE_MARK))
}

/// The mention that calls Codex's GitHub connector, the start of the
/// steward's own ask (burrow-review-ask's `ASK`, which a test holds this to).
pub const MENTION: &str = "@codex";

/// Whether a PR's title or body writes the connector's mention, as GitHub
/// reads a login: in any case, not inside an email or a longer login. Code
/// spans and blocks count: the connector reads the text as written, and of
/// the owner's PRs whose bodies wrote it only in backticks, none got a
/// review of its own (burrow#1293).
pub fn mentions(text: &str) -> bool {
    static MENTIONS: LazyLock<Regex> = LazyLock::new(|| {
        re(&format!(
            "(?i)(?:^|[^A-Za-z0-9_]){}(?:$|[^A-Za-z0-9-])",
            regex::escape(MENTION)
        ))
    });
    MENTIONS.is_match(text)
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

/// Whether a path is a test file by its name or directory: test files are
/// exempt from `Rule::FileSize`.
pub fn is_test(path: &str) -> bool {
    let wrapped = format!("/{path}");
    if wrapped.contains("/tests/") || wrapped.contains("/src/test/") {
        return true;
    }
    let name = path.rsplit('/').next().unwrap_or(path);
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || (name.starts_with("test_") && name.ends_with(".py"))
        || name.ends_with("_test.py")
        || name.ends_with("_test.go")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".test.js")
        || name.ends_with(".test.jsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
        || name.ends_with(".spec.js")
        || name.ends_with(".spec.jsx")
}

/// How many lines `content` has, a last one with no newline included.
pub fn line_count(content: &[u8]) -> usize {
    let ends = content.last().is_some_and(|b| *b != b'\n');
    content.iter().filter(|b| **b == b'\n').count() + usize::from(ends)
}

/// How many non-test code lines a file has for `Rule::FileSize`. For Rust
/// files, lines belonging solely to inline `#[cfg(test)]` items don't count.
pub fn code_lines(path: &str, content: &[u8]) -> usize {
    if !path.ends_with(".rs") || content.is_empty() {
        return line_count(content);
    }
    crate::rust_cfg_test::code_lines(content)
}

/// Standing per-path waivers from an action input: one path or glob pattern
/// per line (`*` and `?` within a segment, `**` for zero or more segments),
/// `#` starting a comment, blank lines ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AllowList {
    patterns: Vec<Vec<String>>,
}

impl AllowList {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let mut patterns = Vec::new();
        for line in raw.lines() {
            let entry = line.split('#').next().unwrap_or("").trim();
            if entry.is_empty() {
                continue;
            }
            if entry.starts_with('/') || entry.ends_with('/') {
                return Err(format!(
                    "allow pattern {entry:?} must be a relative path without leading or trailing '/'"
                ));
            }
            let mut segs: Vec<String> = Vec::new();
            for seg in entry.split('/') {
                if seg.is_empty() || seg == "." || seg == ".." {
                    return Err(format!(
                        "allow pattern {entry:?} has an empty or relative segment"
                    ));
                }
                if seg.contains("**") && seg != "**" {
                    return Err(format!(
                        "allow pattern {entry:?} can only use '**' as a whole path segment"
                    ));
                }
                if seg == "**" && segs.last().map(String::as_str) == Some("**") {
                    continue;
                }
                segs.push(seg.to_owned());
            }
            patterns.push(segs);
        }
        Ok(Self { patterns })
    }

    pub fn matches(&self, path: &str) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let segs: Vec<&str> = path.split('/').collect();
        self.patterns
            .iter()
            .any(|pat| match_path_segments(pat, &segs))
    }
}

fn match_path_segments(pat: &[String], segs: &[&str]) -> bool {
    let Some((first, rest)) = pat.split_first() else {
        return segs.is_empty();
    };
    if first == "**" {
        if rest.is_empty() {
            return true;
        }
        for split in 0..=segs.len() {
            if match_path_segments(rest, &segs[split..]) {
                return true;
            }
        }
        return false;
    }
    let Some((seg, seg_rest)) = segs.split_first() else {
        return false;
    };
    glob_segment(first, seg) && match_path_segments(rest, seg_rest)
}

fn glob_segment(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut star = None;
    while t < text.len() {
        if pattern.get(p) == Some(&'*') {
            star = Some((p, t));
            p += 1;
        } else if pattern.get(p).is_some_and(|&c| c == '?' || c == text[t]) {
            p += 1;
            t += 1;
        } else if let Some((sp, st)) = star {
            star = Some((sp, st + 1));
            p = sp + 1;
            t = st + 1;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// How many lines of its own an AGENTS.md may have, outside the house
/// blocks, before a PR that grows it says why: enough for what the repo is,
/// its own rules and where things live, not for the detail those point to.
pub const GUIDE: usize = 100;

/// Whether a path is an agents' guide, `AGENTS.md`, wherever it is.
pub fn is_guide(path: &str) -> bool {
    path.rsplit('/').next() == Some("AGENTS.md")
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
    fn renovates_branches() {
        let renovate = "Some update.\n\n---\n\nThis PR has been generated by [Mend Renovate CLI](https://example).\n<!--renovate-debug:eyJ9-->\n";
        assert!(!renovate_claims(
            "renovate/nix-lock-file-maintenance",
            renovate
        ));
        assert!(renovate_claims("renovate/rate-limit-mid-run", "Fixes #1\n"));
        // Quoting the mark mid-line doesn't make a PR Renovate's.
        assert!(renovate_claims(
            "renovate/x",
            "see `<!--renovate-debug:` in #607"
        ));
        for head in ["renovate-run/x", "jobs/renovate", "deps/renovate/x", "main"] {
            assert!(!renovate_claims(head, ""), "{head}");
        }
    }

    #[test]
    fn mentions_the_connector() {
        for text in [
            "@codex review",
            "Ask with `@codex review` once a head.",
            "posted ```\n@codex review\n``` as the App",
            "cc @Codex.",
            "(@CODEX)",
            "first line\n@codex",
        ] {
            assert!(mentions(text), "{text}");
        }
        for text in [
            "the connector's bare review mention",
            "Codex reviews every push",
            "mail ops@codex.example",
            "@codex-bot is another login",
            "@codexes",
            "",
        ] {
            assert!(!mentions(text), "{text}");
        }
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
