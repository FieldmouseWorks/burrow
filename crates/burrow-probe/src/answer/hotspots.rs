//! `hotspots OWNER/REPO [SINCE]`: rank non-test code files on the repo's
//! default branch by `commits touching the file * current non-test lines`,
//! using `burrow-house`'s own classification and line counting.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use burrow_house::rules::{self, Language};

use super::{Machine, utc};

/// The most files one hotspot report lists, keeping answers bounded.
pub const LIMIT: usize = 30;

/// One ranked hotspot entry.
#[derive(Debug, PartialEq, Eq)]
struct Entry {
    path: String,
    commits: usize,
    lines: usize,
    score: u64,
}

/// Candidate file at the default branch tip that was touched in the window.
struct Candidate {
    path: String,
    oid: String,
    commits: usize,
    /// `Some(lang)` when the filename's extension determined language; `None`
    /// when extensionless and a shebang check on the blob is needed.
    named_language: Option<Option<Language>>,
}

pub(super) fn answer(m: &Machine, repo: &str, since: i64) -> Result<String, String> {
    let dir = find_repo(m, repo)?;
    let dir = dir
        .to_str()
        .ok_or_else(|| format!("local path for {repo} is not valid UTF-8"))?;
    let (branch, tip) = resolve_default_branch(m, repo, dir)?;
    let shallow = is_shallow(m, dir)?;

    let since_arg = format!("--since=@{since}");
    let log = git_bytes(
        m,
        dir,
        &[
            "log",
            "--first-parent",
            "-m",
            "--no-renames",
            &since_arg,
            "--format=%x1e%H",
            "--name-only",
            "-z",
            &tip,
        ],
        &[],
    )?;
    if log.code != Some(0) {
        return Err(git_error("log", &log.stderr, log.code));
    }
    let (commit_count, touches) = parse_log(&log.stdout);
    let short_tip = &tip[..tip.len().min(12)];
    let shallow_note = if shallow { ", shallow history" } else { "" };

    if commit_count == 0 || touches.is_empty() {
        return Ok(format!(
            "{repo} ({branch} @ {short_tip}{shallow_note}), since {} ({commit_count} commits, 0 code files touched):\nnone\n",
            utc(since)
        ));
    }

    let tree = git_bytes(m, dir, &["ls-tree", "-r", "-z", &tip], &[])?;
    if tree.code != Some(0) {
        return Err(git_error("ls-tree", &tree.stderr, tree.code));
    }
    let candidates = collect_candidates(&tree.stdout, &touches);
    if candidates.is_empty() {
        return Ok(format!(
            "{repo} ({branch} @ {short_tip}{shallow_note}), since {} ({commit_count} commits, 0 code files touched):\nnone\n",
            utc(since)
        ));
    }

    let blobs = read_blobs(m, dir, &candidates)?;
    let mut entries = Vec::new();
    for c in candidates {
        let Some(content) = blobs.get(&c.oid) else {
            continue;
        };
        let language = match c.named_language {
            Some(lang) => lang,
            None => rules::by_shebang(content),
        };
        if !rules::is_code(&c.path, language) {
            continue;
        }
        let lines = rules::code_lines(&c.path, content);
        if lines == 0 {
            continue;
        }
        let score = (c.commits as u64).saturating_mul(lines as u64);
        entries.push(Entry {
            path: c.path,
            commits: c.commits,
            lines,
            score,
        });
    }

    entries.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| b.commits.cmp(&a.commits))
            .then_with(|| b.lines.cmp(&a.lines))
            .then_with(|| a.path.cmp(&b.path))
    });

    Ok(format_report(
        repo,
        &branch,
        short_tip,
        shallow_note,
        since,
        commit_count,
        &entries,
    ))
}

fn git_args<'a>(dir: &'a str, args: &[&'a str]) -> Vec<&'a str> {
    let mut words = Vec::with_capacity(6 + args.len());
    words.extend([
        "--no-lazy-fetch",
        "--no-replace-objects",
        "-c",
        "core.commitGraph=false",
        "-C",
        dir,
    ]);
    words.extend_from_slice(args);
    words
}

fn git_text(m: &Machine, dir: &str, args: &[&str]) -> Result<super::Ran, String> {
    let words = git_args(dir, args);
    m.run.history(m.cfg.tools.git.as_path(), &words, m.uid)
}

fn git_bytes(
    m: &Machine,
    dir: &str,
    args: &[&str],
    stdin: &[u8],
) -> Result<super::RanBytes, String> {
    let words = git_args(dir, args);
    m.run
        .history_bytes(m.cfg.tools.git.as_path(), &words, stdin, m.uid)
}

fn git_error(subcommand: &str, stderr: &str, code: Option<i32>) -> String {
    let why = stderr.trim();
    let how = code.map_or("was killed".to_string(), |c| format!("exited {c}"));
    if why.is_empty() {
        format!("git {subcommand} {how}")
    } else {
        format!("git {subcommand} {how}: {why}")
    }
}

/// Find the best local checkout for `OWNER/REPO` under `m.cfg.projects`.
/// Prefers a primary clone whose directory matches the repo name over linked
/// worktrees or secondary clones.
fn find_repo(m: &Machine, repo: &str) -> Result<PathBuf, String> {
    let projects = m
        .cfg
        .projects
        .as_ref()
        .ok_or("no project histories configured")?;
    let repo_name = repo.split_once('/').map_or(repo, |(_, name)| name);
    let expected = [
        format!("https://github.com/{repo}"),
        format!("git@github.com:{repo}"),
        format!("ssh://git@github.com/{repo}"),
    ];
    let mut matches: Vec<(bool, bool, bool, PathBuf)> = Vec::new();
    for entry in fs::read_dir(projects).map_err(|e| e.to_string())?.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(dir) = path.to_str() else { continue };
        let Ok(remote) = git_text(m, dir, &["remote", "get-url", "origin"]) else {
            continue;
        };
        if remote.code != Some(0) {
            continue;
        }
        let origin = remote.stdout.trim().trim_end_matches(".git");
        if !expected.iter().any(|r| r.eq_ignore_ascii_case(origin)) {
            continue;
        }
        let is_primary = path.join(".git").is_dir();
        let name_matches = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.eq_ignore_ascii_case(repo_name));
        matches.push((
            !(name_matches && is_primary),
            !is_primary,
            !name_matches,
            path,
        ));
    }
    matches.sort();
    matches
        .into_iter()
        .next()
        .map(|(_, _, _, path)| path)
        .ok_or_else(|| format!("no local history for {repo}"))
}

/// Resolve the repo's default branch ref and tip commit SHA.
fn resolve_default_branch(m: &Machine, repo: &str, dir: &str) -> Result<(String, String), String> {
    let mut refs: Vec<String> = Vec::with_capacity(5);
    if let Ok(sym) = git_text(m, dir, &["symbolic-ref", "-q", "refs/remotes/origin/HEAD"])
        && sym.code == Some(0)
    {
        let r = sym.stdout.trim();
        if !r.is_empty() {
            refs.push(r.to_owned());
        }
    }
    for fallback in [
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
        "refs/heads/main",
        "refs/heads/master",
    ] {
        if !refs.iter().any(|r| r == fallback) {
            refs.push(fallback.to_owned());
        }
    }
    for refname in refs {
        let target = format!("{refname}^{{commit}}");
        let rev = git_text(
            m,
            dir,
            &["rev-parse", "--verify", "-q", "--end-of-options", &target],
        )?;
        if rev.code == Some(0) {
            let sha = rev.stdout.trim();
            if sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                let display = refname
                    .strip_prefix("refs/remotes/")
                    .or_else(|| refname.strip_prefix("refs/heads/"))
                    .unwrap_or(&refname);
                return Ok((display.to_owned(), sha.to_owned()));
            }
        }
    }
    Err(format!(
        "no default branch found in local history for {repo}"
    ))
}

fn is_shallow(m: &Machine, dir: &str) -> Result<bool, String> {
    let r = git_text(m, dir, &["rev-parse", "--is-shallow-repository"])?;
    Ok(r.code == Some(0) && r.stdout.trim() == "true")
}

/// Parse `git log --format=%x1e%H --name-only -z` output into total unique
/// commits and per-path commit counts.
fn parse_log(bytes: &[u8]) -> (usize, HashMap<String, usize>) {
    let mut commit_files: HashMap<&str, HashSet<&str>> = HashMap::new();
    for record in bytes.split(|&b| b == 0x1e).skip(1) {
        let Some(nul) = record.iter().position(|&b| b == 0) else {
            continue;
        };
        let Ok(sha) = std::str::from_utf8(&record[..nul]) else {
            continue;
        };
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let seen = commit_files.entry(sha).or_default();
        let rest = &record[nul + 1..];
        let rest = rest.strip_prefix(b"\n").unwrap_or(rest);
        for raw_path in rest.split(|&b| b == 0) {
            if raw_path.is_empty() {
                continue;
            }
            if let Ok(path) = std::str::from_utf8(raw_path) {
                seen.insert(path);
            }
        }
    }
    let commit_count = commit_files.len();
    let mut touches: HashMap<String, usize> = HashMap::new();
    for files in commit_files.into_values() {
        for path in files {
            *touches.entry(path.to_owned()).or_insert(0) += 1;
        }
    }
    (commit_count, touches)
}

/// Filter `git ls-tree -r -z` entries to regular blobs currently on the tip
/// that were touched in the window and pass the house rules' test/extension
/// pre-filter.
fn collect_candidates(tree: &[u8], touches: &HashMap<String, usize>) -> Vec<Candidate> {
    let mut out = Vec::new();
    for entry in tree.split(|&b| b == 0) {
        if entry.is_empty() {
            continue;
        }
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else {
            continue;
        };
        let Ok(meta) = std::str::from_utf8(&entry[..tab]) else {
            continue;
        };
        let Ok(path) = std::str::from_utf8(&entry[tab + 1..]) else {
            continue;
        };
        let mut parts = meta.split_ascii_whitespace();
        let (Some(mode), Some(kind), Some(oid)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if kind != "blob" || !matches!(mode, "100644" | "100755") {
            continue;
        }
        let Some(&commits) = touches.get(path) else {
            continue;
        };
        if rules::is_test(path) {
            continue;
        }
        let named_language = rules::by_name(path);
        if let Some(lang) = named_language
            && !rules::is_code(path, lang)
        {
            continue;
        }
        out.push(Candidate {
            path: path.to_owned(),
            oid: oid.to_owned(),
            commits,
            named_language,
        });
    }
    out
}

/// Batch-read every unique candidate blob OID in one `git cat-file --batch` call.
fn read_blobs(
    m: &Machine,
    dir: &str,
    candidates: &[Candidate],
) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut seen = HashSet::new();
    let mut stdin = Vec::new();
    for c in candidates {
        if seen.insert(c.oid.as_str()) {
            stdin.extend_from_slice(c.oid.as_bytes());
            stdin.push(b'\n');
        }
    }
    if stdin.is_empty() {
        return Ok(HashMap::new());
    }
    let batch = git_bytes(m, dir, &["cat-file", "--batch"], &stdin)?;
    if batch.code != Some(0) {
        return Err(git_error("cat-file", &batch.stderr, batch.code));
    }
    parse_cat_file_batch(&batch.stdout)
}

fn parse_cat_file_batch(buf: &[u8]) -> Result<HashMap<String, Vec<u8>>, String> {
    let mut blobs = HashMap::new();
    let mut pos = 0usize;
    while pos < buf.len() {
        let Some(rel_nl) = buf[pos..].iter().position(|&b| b == b'\n') else {
            return Err("truncated git cat-file --batch header".into());
        };
        let header = std::str::from_utf8(&buf[pos..pos + rel_nl])
            .map_err(|_| "non-UTF-8 git cat-file --batch header".to_string())?;
        pos += rel_nl + 1;
        if header.ends_with(" missing") {
            continue;
        }
        let mut parts = header.split_ascii_whitespace();
        let (Some(oid), Some(kind), Some(size_str)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(format!("unexpected git cat-file header: {header:?}"));
        };
        let size: usize = size_str
            .parse()
            .map_err(|_| format!("invalid git cat-file size: {size_str:?}"))?;
        let end = pos
            .checked_add(size)
            .filter(|&e| e <= buf.len())
            .ok_or_else(|| "truncated git cat-file --batch payload".to_string())?;
        if kind == "blob" {
            blobs.insert(oid.to_owned(), buf[pos..end].to_vec());
        }
        pos = end;
        if buf.get(pos) == Some(&b'\n') {
            pos += 1;
        }
    }
    Ok(blobs)
}

fn format_report(
    repo: &str,
    branch: &str,
    short_tip: &str,
    shallow_note: &str,
    since: i64,
    commit_count: usize,
    entries: &[Entry],
) -> String {
    let total = entries.len();
    if total == 0 {
        return format!(
            "{repo} ({branch} @ {short_tip}{shallow_note}), since {} ({commit_count} commits, 0 code files touched):\nnone\n",
            utc(since)
        );
    }
    let shown = total.min(LIMIT);
    let top = &entries[..shown];
    let scope = if total > shown {
        format!("{commit_count} commits, {total} code files touched, top {shown}")
    } else {
        format!("{commit_count} commits, {total} code files touched")
    };
    let mut out = format!(
        "{repo} ({branch} @ {short_tip}{shallow_note}), since {} ({scope}):\n",
        utc(since)
    );
    let score_w = top
        .iter()
        .map(|e| e.score.to_string().len())
        .max()
        .unwrap_or(0)
        .max("score".len());
    let commits_w = top
        .iter()
        .map(|e| e.commits.to_string().len())
        .max()
        .unwrap_or(0)
        .max("commits".len());
    let lines_w = top
        .iter()
        .map(|e| e.lines.to_string().len())
        .max()
        .unwrap_or(0)
        .max("lines".len());
    out.push_str(&format!(
        "  {:>score_w$}  {:>commits_w$}  {:>lines_w$}  path\n",
        "score", "commits", "lines"
    ));
    for e in top {
        out.push_str(&format!(
            "  {:>score_w$}  {:>commits_w$}  {:>lines_w$}  {}\n",
            e.score,
            e.commits,
            e.lines,
            escape_path(&e.path)
        ));
    }
    if total > shown {
        out.push_str(&format!("({} more code files omitted)\n", total - shown));
    }
    out
}

fn escape_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for c in path.chars() {
        if c.is_control() {
            for esc in c.escape_default() {
                out.push(esc);
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
#[path = "hotspots_tests.rs"]
mod tests;
