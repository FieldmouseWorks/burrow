//! The `first` label (burrow.autoland.labels.first) on an issue or PR puts
//! its work ahead of the line, and counts only when one of the owners put it
//! on, as GitHub names the account that did. A Claude job for a person's
//! issue acts on GitHub as that person, so GitHub can't tell its label from
//! theirs: here a proj job's commands that would put the label on are
//! refused (asked about, which a job can't answer), whoever the job acts
//! as, so no job moves work up the line, its own or any other.
//!
//! What puts it on: `gh issue|pr edit --add-label`, `gh issue|pr create
//! --label` (or `new`, gh's alias, and an issue template's labels or a
//! recovered run's), a label renamed to it, and a write through GitHub's
//! API, `gh api`'s or curl's, at an issue, its labels, or a repo's issues (a
//! new one's labels) whose labels name it, or GraphQL's label writes, which
//! name labels by id. A gh alias could be any of them, so a job doesn't
//! make one.
//!
//! Each command is read as gh and curl read their words (gh.rs, curl.rs),
//! with the shell's expansions told from literal text (walk.rs's marked
//! reading). What the guard can't read there counts as naming it: a label,
//! an endpoint, a method, a labels field, a GraphQL query or a body that
//! only an expansion, a substitution, a glob or a file gives, an unquoted
//! one or a glob anywhere in such a command (it may split into any words,
//! options among them), a word that starts with one where an option may
//! be, words `xargs` adds, and curl's default config. Elsewhere an
//! expansion is left alone, so a job still posts a comment or a PR body
//! from a quoted variable or substitution. It reads a command as a job
//! means it; a program or script that writes the label itself isn't a
//! command it sees.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::codex::{endpoint, program};
use crate::curl::{self, Body, Curl};
use crate::gh::{self, Args, Spec};
use crate::{pattern, shell, walk};

/// Why.
fn reason(label: &str) -> String {
    format!(
        "Puts the `{label}` label on an issue or PR, or may (a label, endpoint, method, query or body the guard can't read: an unquoted expansion, one where an option may be, xargs, a file, a template; or GraphQL's label ids). It counts only when the owner puts it on, and this job would put it on as them or as an App: no job moves work up the line, its own or any other. Leave it to the owner, or spell the command out: quote expansions, name the labels you mean on the line, and give a body with --body-file."
    )
}

/// Where GitHub's REST API puts labels on: an issue (or PR, which is one)
/// with `labels`, its labels, or a new issue.
static LABELLED: LazyLock<Regex> = LazyLock::new(|| pattern(r"(^|/)issues(/[^/]+(/labels)?)?$"));

/// Where GitHub's REST API renames a label: one of a repo's labels.
static A_LABEL: LazyLock<Regex> = LazyLock::new(|| pattern(r"(^|/)labels/[^/]+$"));

/// GitHub's GraphQL label writes, which name labels by id (or rename one,
/// or take an issue template's), case and all as GraphQL reads them.
static LABEL_IDS: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"addLabelsToLabelable|labelIds|issueTemplate|updateLabel"));

/// An endpoint as GitHub routes it: its path's `.` and `..` segments
/// resolved and its `%` escapes decoded, no query or trailing slash.
fn route(url: &str) -> String {
    let path = endpoint(url);
    let mut decoded = Vec::with_capacity(path.len());
    let bytes = path.as_bytes();
    let hex = |b: Option<&u8>| b.and_then(|b| char::from(*b).to_digit(16));
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let (Some(h), Some(l)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2)))
        {
            decoded.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
            i += 3;
            continue;
        }
        decoded.push(bytes[i]);
        i += 1;
    }
    let path = String::from_utf8_lossy(&decoded);
    let mut segments: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "." => {}
            ".." => {
                segments.pop();
            }
            s => segments.push(s),
        }
    }
    segments.join("/").trim_end_matches('/').to_owned()
}

/// What a write at an endpoint may do, for the label.
#[derive(Clone, Copy)]
enum At {
    /// An issue, its labels, or a new issue (REST).
    Labelled,
    /// A repo's label, which a write may rename (REST).
    Label,
    /// GitHub's GraphQL.
    Graphql,
    /// Anywhere else: nothing to do with labels on issues.
    Elsewhere,
}

impl At {
    fn of(url: &str) -> At {
        // GitHub routes a path whatever its case.
        let path = route(url).to_lowercase();
        if path.ends_with("graphql") {
            At::Graphql
        } else if LABELLED.is_match(&path) {
            At::Labelled
        } else if A_LABEL.is_match(&path) {
            At::Label
        } else {
            At::Elsewhere
        }
    }
}

/// Whether two label names are the same to GitHub, whatever their case.
fn same(a: &str, b: &str) -> bool {
    a.trim().to_lowercase() == b.trim().to_lowercase()
}

/// Whether a list of label names, as gh takes them (pflag's string slice:
/// one CSV record), names `label`, or may: any of its fields read as CSV
/// does, quotes and all, or as plain `,`-separated text, whichever gh
/// makes of it. One the guard can't read may.
fn names(list: &str, label: &str) -> bool {
    shell::unread(list)
        || csv_fields(list).iter().any(|l| same(l, label))
        || list.split(',').any(|l| same(&l.replace('"', ""), label))
}

/// A CSV record's fields, as Go's encoding/csv reads its first line: a
/// field that starts with `"` is quoted, `""` in it a `"`.
fn csv_fields(record: &str) -> Vec<String> {
    let record = record.split(['\n', '\r']).next().unwrap_or("");
    let mut fields = Vec::new();
    let mut chars = record.chars().peekable();
    loop {
        let mut field = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                if c == '"' {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        break;
                    }
                } else {
                    field.push(c);
                }
            }
            // Anything up to the next comma, as lenient readers keep it.
            for c in chars.by_ref() {
                if c == ',' {
                    break;
                }
                field.push(c);
            }
            fields.push(field);
            if chars.peek().is_none() && !record.ends_with(',') {
                return fields;
            }
            continue;
        }
        for c in chars.by_ref() {
            if c == ',' {
                fields.push(std::mem::take(&mut field));
                break;
            }
            field.push(c);
        }
        if chars.peek().is_none() {
            fields.push(field);
            return fields;
        }
    }
}

/// Whether a request body (JSON, or `key=value&…`) gives `labels` that
/// name `label`, or, at a repo's label, renames it to `label`. One the
/// guard can read neither way may.
fn body_names(body: &str, label: &str, at: At) -> bool {
    let named = |v: &Value| match v {
        Value::String(s) => same(s, label),
        Value::Object(o) => o.iter().any(|(k, v)| {
            k.eq_ignore_ascii_case("name") && v.as_str().is_some_and(|s| same(s, label))
        }),
        _ => false,
    };
    let in_list = |v: &Value| match v {
        Value::Array(a) => a.iter().any(named),
        v => named(v),
    };
    let key = |k: &str| match at {
        At::Label => k.eq_ignore_ascii_case("new_name") || k.eq_ignore_ascii_case("name"),
        _ => k.to_ascii_lowercase().starts_with("labels"),
    };
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Array(a)) => a.iter().any(named),
        Ok(Value::Object(o)) => o.iter().any(|(k, v)| key(k) && in_list(v)),
        Ok(_) => false,
        Err(_) if body.contains('=') => body.split('&').any(|pair| {
            pair.split_once('=')
                .is_some_and(|(k, v)| key(k) && names(v, label))
        }),
        Err(_) => body.to_lowercase().contains(&label.to_lowercase()),
    }
}

/// Whether a write through GitHub's API (curl.rs's [`curl::Counts`]) puts
/// `label` on, or may. One whose endpoint the guard can't read may be any.
fn writes_label(label: &str, endpoint: Option<&str>, body: &Body, unread_body: bool) -> bool {
    let rest = |at| unread_body || body.pieces.iter().any(|b| body_names(b, label, at));
    let graphql = || unread_body || body.texts().any(|b| LABEL_IDS.is_match(b));
    match endpoint.map(At::of) {
        None => rest(At::Labelled) || rest(At::Label) || graphql(),
        Some(At::Graphql) => graphql(),
        Some(at @ (At::Labelled | At::Label)) => rest(at),
        Some(At::Elsewhere) => false,
    }
}

/// Whether a word starts with what the guard can't read, so may be an
/// option, whatever it reads as.
fn may_be_an_option(word: &str) -> bool {
    word.starts_with([shell::UNKNOWN, shell::SPLITS]) || shell::globs(word)
}

/// The options of `gh issue|pr edit|create` that take a value: all four
/// commands' (an option one of them doesn't have is an error there).
const EDIT: Spec = Spec {
    values: &[
        "add-assignee",
        "add-blocked-by",
        "add-blocking",
        "add-label",
        "add-project",
        "add-reviewer",
        "add-sub-issue",
        "assignee",
        "attach",
        "base",
        "blocked-by",
        "blocking",
        "body",
        "body-file",
        "head",
        "label",
        "milestone",
        "parent",
        "project",
        "recover",
        "remove-assignee",
        "remove-blocked-by",
        "remove-blocking",
        "remove-label",
        "remove-project",
        "remove-reviewer",
        "remove-sub-issue",
        "repo",
        "reviewer",
        "template",
        "title",
        "type",
    ],
    shorts: &[
        ('a', "assignee", true),
        ('B', "base", true),
        ('b', "body", true),
        ('F', "body-file", true),
        ('H', "head", true),
        ('l', "label", true),
        ('m', "milestone", true),
        ('p', "project", true),
        ('r', "reviewer", true),
        ('R', "repo", true),
        ('T', "template", true),
        ('t', "title", true),
        ('d', "draft", false),
        ('e', "editor", false),
        ('f', "fill", false),
        ('w', "web", false),
    ],
};

/// What a gh command line is, for the label.
enum Command {
    /// `issue|pr edit`.
    Edit,
    /// `issue|pr create` (or `new`); whether it's an issue's.
    Create { issue: bool },
    /// `label edit`, which may rename a label.
    LabelEdit,
    /// `api`.
    Api,
    /// `alias set|import`: a new command, which may be any of them.
    Alias,
    /// What the guard can't name: its command words come from an
    /// expansion, or from xargs.
    Unknown,
    /// Anything else.
    Other,
}

fn command(words: &[&str], more: bool) -> Command {
    if words.iter().copied().any(shell::unread) {
        return Command::Unknown;
    }
    match words {
        ["api", ..] => Command::Api,
        ["issue" | "pr", "edit"] => Command::Edit,
        [noun @ ("issue" | "pr"), "create" | "new"] => Command::Create {
            issue: *noun == "issue",
        },
        ["label", "edit"] => Command::LabelEdit,
        ["alias", "set" | "import"] => Command::Alias,
        [_] | [] if more => Command::Unknown,
        _ => Command::Other,
    }
}

/// Whether `gh`'s words put `label` on, or may. `sure` is whether it's gh
/// by name; a word the guard can't read may be gh too, but only what's
/// plainly gh's (its command, its options) counts there.
fn gh(words: &[&str], more: bool, label: &str, sure: bool) -> bool {
    let owned: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    let read = gh::read(owned.iter());
    let rest = &read.rest;
    match command(&read.command, more) {
        Command::Unknown | Command::Alias => sure,
        Command::Other => false,
        Command::Api => api(rest, more, label),
        Command::LabelEdit => {
            more || rest.iter().copied().any(shell::splits) || {
                let args = gh::args(rest, &LABEL_EDIT);
                args.opts.iter().any(|o| {
                    shell::unread(o.name)
                        || (o.name == "name" && o.value.is_some_and(|v| names(v, label)))
                }) || args.positional.iter().any(|w| may_be_an_option(w))
            }
        }
        Command::Edit => {
            more || rest.iter().copied().any(shell::splits)
                || edit(&gh::args(rest, &EDIT), false, label)
        }
        Command::Create { issue } => {
            more || rest.iter().copied().any(shell::splits)
                || edit(&gh::args(rest, &EDIT), issue, label)
        }
    }
}

/// `gh label edit`'s options that take a value.
const LABEL_EDIT: Spec = Spec {
    values: &["color", "description", "name", "repo"],
    shorts: &[
        ('c', "color", true),
        ('d', "description", true),
        ('n', "name", true),
        ('R', "repo", true),
    ],
};

/// Whether `gh issue|pr edit|create`'s options put `label` on, or may: a
/// label that names it or that the guard can't read, an option it can't
/// name, a recovered run's labels, or an issue template's.
fn edit(args: &Args<'_>, issue: bool, label: &str) -> bool {
    args.opts.iter().any(|o| match o.name {
        "add-label" | "label" => o.value.is_none_or(|v| names(v, label)),
        "recover" => true,
        "template" => issue,
        name => shell::unread(name),
    }) || args.positional.iter().any(|w| may_be_an_option(w))
}

/// `{owner}`, `{repo}` or `{branch}`, which gh fills in a typed field.
fn placeholder(value: &str) -> bool {
    ["{owner}", "{repo}", "{branch}"]
        .iter()
        .any(|p| value.contains(p))
}

/// Whether `gh api`'s words put `label` on, or may.
fn api(words: &[&str], more: bool, label: &str) -> bool {
    let args = gh::args(words, &gh::API);
    // Words the guard can't read where they'd change the call: xargs's,
    // an unquoted expansion or a glob, an option it can't name, or an
    // expansion where an option may be (beside the endpoint, it may be one).
    if more
        || words.iter().copied().any(shell::splits)
        || args.opts.iter().any(|o| shell::unread(o.name))
        || (args.positional.len() > 1 && args.positional.iter().any(|w| may_be_an_option(w)))
    {
        return true;
    }
    let mut method = None;
    let mut fields = Vec::new();
    let mut input = false;
    for o in &args.opts {
        match o.name {
            "method" => method = o.value,
            "field" | "raw-field" => fields.extend(o.value.map(|v| (v, o.name == "field"))),
            "input" => input = true,
            _ => {}
        }
    }
    let writes = match method {
        Some(m) => {
            shell::unread(m) || !(m.eq_ignore_ascii_case("GET") || m.eq_ignore_ascii_case("HEAD"))
        }
        None => !fields.is_empty() || input,
    };
    if !writes {
        return false;
    }
    // A typed value gh reads from a file or fills in.
    let unread_value = |v: &str, typed: bool| {
        shell::unread(v) || (typed && (v.starts_with('@') || placeholder(v)))
    };
    let rest = |at: At| {
        input
            || fields.iter().any(|&(text, typed)| {
                let (key, value) = text.split_once('=').unwrap_or((text, ""));
                let names_it = |k: &str| match at {
                    At::Label => {
                        k.eq_ignore_ascii_case("new_name") || k.eq_ignore_ascii_case("name")
                    }
                    _ => k.to_ascii_lowercase().starts_with("labels"),
                };
                shell::unread(key)
                    || (names_it(key) && (unread_value(value, typed) || names(value, label)))
            })
    };
    let graphql = || {
        input
            || fields.iter().any(|&(text, typed)| {
                let (key, value) = text.split_once('=').unwrap_or((text, ""));
                shell::unread(key)
                    || LABEL_IDS.is_match(text)
                    || (key == "query" && unread_value(value, typed))
            })
    };
    match args.positional.as_slice() {
        [url] if !shell::unread(url) && !placeholder_branch(url) => match At::of(url) {
            At::Graphql => graphql(),
            at @ (At::Labelled | At::Label) => rest(at),
            At::Elsewhere => false,
        },
        // Where it goes, the guard can't read: anywhere.
        _ => rest(At::Labelled) || rest(At::Label) || graphql(),
    }
}

/// Whether an endpoint holds `{branch}`, which gh fills in with a name
/// that may hold `/`, so any path.
fn placeholder_branch(url: &str) -> bool {
    url.contains("{branch}")
}

/// Whether curl's words put `label` on, or may. `sure` as for [`gh`].
fn curl(words: &[&str], more: bool, cmd: &str, label: &str, sure: bool) -> bool {
    if sure && (more || words.iter().copied().any(shell::splits)) {
        return true;
    }
    let counts = |endpoint: Option<&str>, body: &Body, unread: bool| {
        writes_label(label, endpoint, body, unread)
    };
    match curl::read(words, &counts) {
        Curl::Fine => sure && curl::reads_default_config(words, cmd),
        Curl::Writes | Curl::Config => true,
    }
}

/// The refusal for a command of a job's that puts `label` on, if it does.
pub fn refusals(cmd: &str, label: &str) -> Vec<String> {
    let mut found = false;
    // A command too long or deep to read is push.rs's question.
    let _ = walk::walk_marked(cmd, &mut |word, rest, budget, more| {
        let program = program(word);
        // Only what may be gh or curl is read on: every word is visited.
        if !matches!(program, "gh" | "curl") && !shell::unread(word) {
            return Ok(());
        }
        let words: Vec<&str> = rest.iter(budget).map(String::as_str).collect();
        found |= match program {
            "gh" => gh(&words, more, label, true),
            "curl" => curl(&words, more, cmd, label, true),
            // An expansion may be gh or curl; unquoted, it may be gh and
            // its command too, so its options count wherever they are.
            _ if shell::unread(word) => {
                gh(&words, more, label, false)
                    || curl(&words, more, cmd, label, false)
                    || (shell::splits(word) && tail(&words, label))
            }
            _ => false,
        };
        Ok(())
    });
    if found {
        vec![reason(label)]
    } else {
        Vec::new()
    }
}

/// Whether words that may follow any gh command plainly put `label` on:
/// as an edit's or a create's options, or as `gh api`'s.
fn tail(words: &[&str], label: &str) -> bool {
    let args = gh::args(words, &EDIT);
    args.opts.iter().any(|o| {
        matches!(o.name, "add-label" | "label") && o.value.is_some_and(|v| names(v, label))
    }) || {
        let args = gh::args(words, &gh::API);
        args.opts.iter().any(|o| {
            matches!(o.name, "field" | "raw-field")
                && o.value.is_some_and(|text| {
                    let (key, value) = text.split_once('=').unwrap_or((text, ""));
                    LABEL_IDS.is_match(text)
                        || (key.to_ascii_lowercase().starts_with("labels") && names(value, label))
                })
        })
    }
}
