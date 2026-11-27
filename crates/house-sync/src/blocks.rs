//! The house blocks: one a file, house/NAME.md, its first and last lines
//! its markers, `<!-- house:NAME vN begin ... -->` and
//! `<!-- house:NAME end -->`, and how a repo's AGENTS.md gets them.

use std::fs;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

pub struct Block {
    pub name: String,
    pub text: Vec<u8>,
}

/// The blocks a pass keeps, in the order of their files' names.
pub struct House {
    pub blocks: Vec<Block>,
    /// The first 8 hex digits of the sha256 of `sha256sum -- *.md`'s
    /// output, the name the bash gave the same blocks.
    pub rev: String,
}

impl House {
    pub fn read(dir: &Path) -> io::Result<House> {
        let mut names: Vec<String> = Vec::new();
        for entry in fs::read_dir(dir)? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if name.ends_with(".md") && !name.starts_with('.') {
                names.push(name);
            }
        }
        names.sort();
        if names.is_empty() {
            return Err(io::Error::other(format!(
                "{}: no house blocks in it",
                dir.display()
            )));
        }
        let mut sums = String::new();
        let mut blocks = Vec::new();
        for file in names {
            let text = fs::read(dir.join(&file))?;
            sums.push_str(&format!("{}  {file}\n", hex(&Sha256::digest(&text))));
            let name = file.strip_suffix(".md").expect("an .md file").to_string();
            blocks.push(Block { name, text });
        }
        let rev = hex(&Sha256::digest(sums.as_bytes()))[..8].to_string();
        Ok(House { blocks, rev })
    }

    /// Every block is part of the house rules, so the branch keeps their
    /// name.
    pub fn branch(&self) -> String {
        format!("house/rules-{}", self.rev)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Debug, PartialEq, Eq)]
pub enum Rewritten {
    /// It has none of the blocks yet: its baseline job or the rollout adds
    /// them.
    NoBlock,
    /// This block's markers aren't one begin line, one end line after it,
    /// and no other block's marker between them.
    Mangled(String),
    /// AGENTS.md with each block it has as burrow has it, and everything
    /// outside the markers as it was.
    Text(Vec<u8>),
}

/// The four common P1 lines hand-copied from `docs/repo-baseline.md` before
/// `house/review.md` became a house block.
const COMMON_REVIEW_P1: [&[u8]; 4] = [
    b"- a change that makes a check pass without fixing the thing it checks",
    b"- tests or checks weakened, skipped or deleted to get green",
    b"- secrets, tokens or personal data in the diff or the logs it adds",
    b"- behavior changed beyond what the PR says it does",
];

/// Boilerplate intro lines that preceded [`COMMON_REVIEW_P1`] in the template
/// and existing repos' AGENTS.md.
fn is_standard_review_intro(line: &[u8]) -> bool {
    matches!(
        line,
        b"Flag as P1:"
            | b"For reviewers, the review bot included. Flag as P1:"
            | b"For reviewers, Codex included. Flag as P1:"
            | b"For every reviewer, the steward's included. Flag as P1:"
            | b"For every reviewer, the review bot included. Flag as P1:"
            | b"For every reviewer, Codex included. Flag as P1:"
    )
}

/// AGENTS.md as it should be. A block it doesn't have stays out, except the
/// `review` block, which is placed inside an existing `## Code Review Rules`
/// section carrying the four common P1 lines once the file already carries at
/// least one house block.
pub fn rewrite(agents: &[u8], house: &House) -> Rewritten {
    let mut text = agents.to_vec();
    let mut found = false;
    let mut saw_review = false;
    for block in &house.blocks {
        let lines: Vec<&[u8]> = text.split_inclusive(|&b| b == b'\n').collect();
        let at = |is: &dyn Fn(&[u8]) -> bool| -> Vec<usize> {
            (0..lines.len()).filter(|&i| is(bare(lines[i]))).collect()
        };
        let begins = at(&|l| begins(l, &block.name));
        let ends = at(&|l| ends(l, &block.name));
        if begins.is_empty() && ends.is_empty() {
            continue;
        }
        let (&[b], &[e]) = (begins.as_slice(), ends.as_slice()) else {
            return Rewritten::Mangled(block.name.clone());
        };
        if e <= b || lines[b + 1..e].iter().any(|l| marker(bare(l))) {
            return Rewritten::Mangled(block.name.clone());
        }
        let mut next = lines[..b].concat();
        next.extend_from_slice(&block.text);
        next.extend_from_slice(&lines[e + 1..].concat());
        text = next;
        found = true;
        if block.name == "review" {
            saw_review = true;
        }
    }
    if !found {
        return Rewritten::NoBlock;
    }
    if !saw_review
        && let Some(review) = house.blocks.iter().find(|b| b.name == "review")
        && let Some(placed) = place_review(&text, review)
    {
        text = placed;
    }
    Rewritten::Text(text)
}

/// Places the `review` house block inside a single top-level `## Code Review
/// Rules` section (outside house blocks and fenced code blocks) when the
/// section carries the four [`COMMON_REVIEW_P1`] lines. Standard boilerplate
/// intro lines and the four common P1 lines are replaced in place so the
/// repo's own P1 bullets stay right under `Flag as P1:`; a repo-specific intro
/// paragraph before those four lines is kept directly below the block with the
/// duplicated four lines removed. Sections that customized those four lines
/// (such as private repos that intentionally allow personal data) are left
/// untouched rather than injecting a contradictory P1 rule.
fn place_review(text: &[u8], review: &Block) -> Option<Vec<u8>> {
    let lines: Vec<&[u8]> = text.split_inclusive(|&b| b == b'\n').collect();
    let h = find_review_heading(&lines)?;
    let sec_end = find_section_end(&lines, h);
    let bullets = find_common_review_p1(&lines, h + 1, sec_end)?;
    let tail = bullets + COMMON_REVIEW_P1.len();

    let mut intro_start = h + 1;
    while intro_start < bullets && bare(lines[intro_start]).is_empty() {
        intro_start += 1;
    }
    let mut intro_end = bullets;
    while intro_end > intro_start && bare(lines[intro_end - 1]).is_empty() {
        intro_end -= 1;
    }
    let standard_intro = intro_start == intro_end
        || (intro_end == intro_start + 1 && is_standard_review_intro(bare(lines[intro_start])));

    let mut next = lines[..=h].concat();
    if !next.ends_with(b"\n") {
        next.push(b'\n');
    }
    next.push(b'\n');
    next.extend_from_slice(&review.text);
    if standard_intro {
        if tail < lines.len() {
            let next_line = bare(lines[tail]);
            if !next_line.is_empty() && !next_line.starts_with(b"- ") {
                next.push(b'\n');
            }
            next.extend_from_slice(&lines[tail..].concat());
        }
    } else {
        next.push(b'\n');
        next.extend_from_slice(&lines[intro_start..intro_end].concat());
        if !next.ends_with(b"\n") {
            next.push(b'\n');
        }
        if tail < lines.len() {
            let next_line = bare(lines[tail]);
            if !next_line.is_empty() {
                next.push(b'\n');
            }
            next.extend_from_slice(&lines[tail..].concat());
        }
    }
    Some(next)
}

fn toggle_fence(fence: &mut Option<u8>, line: &[u8]) -> bool {
    let Some(ch) = fence_marker(line) else {
        return false;
    };
    match *fence {
        None => *fence = Some(ch),
        Some(open) if open == ch => *fence = None,
        Some(_) => {}
    }
    true
}

fn find_review_heading(lines: &[&[u8]]) -> Option<usize> {
    let mut headings = Vec::new();
    let mut i = 0;
    let mut fence = None;
    while i < lines.len() {
        let line = bare(lines[i]);
        if fence.is_none()
            && let Some(name) = begin_name(line)
        {
            let name = String::from_utf8_lossy(name);
            if let Some(end) = (i + 1..lines.len())
                .find(|&j| marker(bare(lines[j])))
                .filter(|&j| ends(bare(lines[j]), &name))
            {
                i = end + 1;
                continue;
            }
        }
        if toggle_fence(&mut fence, line) {
            i += 1;
            continue;
        }
        if fence.is_none() && line == b"## Code Review Rules" {
            headings.push(i);
        }
        i += 1;
    }
    let [h] = headings.as_slice() else {
        return None;
    };
    Some(*h)
}

fn find_section_end(lines: &[&[u8]], heading: usize) -> usize {
    let mut fence = None;
    for (j, raw) in lines.iter().enumerate().skip(heading + 1) {
        let line = bare(raw);
        if toggle_fence(&mut fence, line) {
            continue;
        }
        if fence.is_none() && (line.starts_with(b"## ") || line == b"##") {
            return j;
        }
    }
    lines.len()
}

fn find_common_review_p1(lines: &[&[u8]], start: usize, end: usize) -> Option<usize> {
    let mut fence = None;
    let mut k = start;
    while k + COMMON_REVIEW_P1.len() <= end {
        let line = bare(lines[k]);
        if toggle_fence(&mut fence, line) {
            k += 1;
            continue;
        }
        if fence.is_none()
            && (0..COMMON_REVIEW_P1.len()).all(|m| bare(lines[k + m]) == COMMON_REVIEW_P1[m])
        {
            return Some(k);
        }
        k += 1;
    }
    None
}

fn fence_marker(line: &[u8]) -> Option<u8> {
    let trimmed = line.iter().position(|b| !b.is_ascii_whitespace())?;
    let rest = &line[trimmed..];
    if rest.starts_with(b"```") {
        Some(b'`')
    } else if rest.starts_with(b"~~~") {
        Some(b'~')
    } else {
        None
    }
}

/// A line without its newline.
fn bare(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\n").unwrap_or(line)
}

/// `vN begin`, N one or more digits, at the start of `rest`.
fn version_begin(rest: &[u8]) -> bool {
    let Some(rest) = rest.strip_prefix(b"v") else {
        return false;
    };
    let digits = rest.iter().take_while(|b| b.is_ascii_digit()).count();
    digits > 0 && rest[digits..].starts_with(b" begin")
}

/// `<!-- house:NAME vN begin`, and anything after it.
fn begins(line: &[u8], name: &str) -> bool {
    line.strip_prefix(format!("<!-- house:{name} ").as_bytes())
        .is_some_and(version_begin)
}

/// Exactly `<!-- house:NAME end -->`.
fn ends(line: &[u8], name: &str) -> bool {
    line == format!("<!-- house:{name} end -->").as_bytes()
}

/// What follows `<!-- house:NAME ` on a line, for any NAME.
fn after_name(line: &[u8]) -> Option<&[u8]> {
    let rest = line.strip_prefix(b"<!-- house:")?;
    let space = rest.iter().position(|&b| b == b' ').filter(|&s| s > 0)?;
    Some(&rest[space + 1..])
}

/// Any block's begin or end marker.
fn marker(line: &[u8]) -> bool {
    after_name(line).is_some_and(|after| after.starts_with(b"end -->") || version_begin(after))
}

/// The NAME of `<!-- house:NAME vN begin ...`, or None for any other line.
fn begin_name(line: &[u8]) -> Option<&[u8]> {
    let rest = line.strip_prefix(b"<!-- house:")?;
    let space = rest.iter().position(|&b| b == b' ').filter(|&s| s > 0)?;
    version_begin(&rest[space + 1..]).then_some(&rest[..space])
}

/// How many of an AGENTS.md's lines are the repo's own: every line but the
/// house blocks', markers included. A block is what `rewrite` takes for one:
/// a begin marker whose next marker, of any block, is its own name's end.
/// Anything else (a begin with no end, a mismatched or nested marker) opens
/// no block, so its lines stay the repo's.
pub fn own_lines(agents: &[u8]) -> usize {
    let lines: Vec<&[u8]> = agents.split_inclusive(|&b| b == b'\n').map(bare).collect();
    let mut own = 0;
    let mut i = 0;
    while i < lines.len() {
        let closes = begin_name(lines[i]).and_then(|name| {
            let name = String::from_utf8_lossy(name);
            let next = (i + 1..lines.len()).find(|&j| marker(lines[j]))?;
            ends(lines[next], &name).then_some(next)
        });
        match closes {
            Some(end) => i = end + 1,
            None => {
                own += 1;
                i += 1;
            }
        }
    }
    own
}

/// Whether two directories hold the same files, as `diff -rq` finds them:
/// the same names all the way down, each file the same bytes. One that
/// can't be read is different.
pub fn same_tree(a: &Path, b: &Path) -> bool {
    fn names(dir: &Path) -> io::Result<Vec<std::ffi::OsString>> {
        let mut names = fs::read_dir(dir)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        names.sort();
        Ok(names)
    }
    let (Ok(left), Ok(right)) = (names(a), names(b)) else {
        return false;
    };
    left == right
        && left.iter().all(|name| {
            let (x, y) = (a.join(name), b.join(name));
            match (fs::metadata(&x), fs::metadata(&y)) {
                (Ok(m), Ok(n)) if m.is_dir() && n.is_dir() => same_tree(&x, &y),
                (Ok(m), Ok(n)) if !m.is_dir() && !n.is_dir() => {
                    matches!((fs::read(&x), fs::read(&y)), (Ok(p), Ok(q)) if p == q)
                }
                _ => false,
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn house() -> House {
        House {
            blocks: vec![
                Block {
                    name: "prs".into(),
                    text: b"<!-- house:prs v2 begin. x -->\nPRS\n<!-- house:prs end -->\n".to_vec(),
                },
                Block {
                    name: "rules".into(),
                    text: b"<!-- house:rules v1 begin. -->\nRULES\n<!-- house:rules end -->\n"
                        .to_vec(),
                },
            ],
            rev: "00000000".into(),
        }
    }

    fn text(s: &str) -> Rewritten {
        Rewritten::Text(s.as_bytes().to_vec())
    }

    #[test]
    fn each_block_it_has_as_burrow_has_it() {
        let h = house();
        assert_eq!(
            rewrite(
                b"top\n<!-- house:rules v7 begin -->\nold\n<!-- house:rules end -->\nmid\n<!-- house:prs v1 begin. -->\n<!-- house:prs end -->\nend",
                &h
            ),
            text(
                "top\n<!-- house:rules v1 begin. -->\nRULES\n<!-- house:rules end -->\nmid\n<!-- house:prs v2 begin. x -->\nPRS\n<!-- house:prs end -->\nend"
            )
        );
        // Only the ones it has.
        assert_eq!(
            rewrite(b"<!-- house:prs v1 begin -->\n<!-- house:prs end -->", &h),
            text("<!-- house:prs v2 begin. x -->\nPRS\n<!-- house:prs end -->\n")
        );
        assert_eq!(rewrite(b"# AGENTS.md\n", &h), Rewritten::NoBlock);
        assert_eq!(rewrite(b"", &h), Rewritten::NoBlock);
        // Lines that aren't its markers: another name, no version, an end
        // marker with more on its line, one not at the start of its line.
        assert_eq!(
            rewrite(
                b"<!-- house:prsx v1 begin -->\n<!-- house:prs v begin -->\n<!-- house:prs end --> x\n <!-- house:prs end -->\n",
                &h
            ),
            Rewritten::NoBlock
        );
    }

    #[test]
    fn mangled_markers_are_named() {
        let h = house();
        for agents in [
            // Twice, a begin with no end, an end with no begin, the end
            // first.
            "<!-- house:rules v1 begin -->\n<!-- house:rules end -->\n<!-- house:rules v1 begin -->\n<!-- house:rules end -->\n",
            "<!-- house:rules v1 begin -->\n",
            "<!-- house:rules end -->\n",
            "<!-- house:rules end -->\n<!-- house:rules v1 begin -->\n",
            // Another block's marker inside, or anyone's.
            "<!-- house:rules v1 begin -->\n<!-- house:new v1 begin -->\n<!-- house:rules end -->\n",
            "<!-- house:rules v1 begin -->\n<!-- house:other end -->\n<!-- house:rules end -->\n",
            "<!-- house:rules v1 begin -->\n<!-- house:x-y v12 begin and more\n<!-- house:rules end -->\n",
        ] {
            assert_eq!(
                rewrite(agents.as_bytes(), &h),
                Rewritten::Mangled("rules".into()),
                "{agents}"
            );
        }
        // The first block found broken is the one named.
        assert_eq!(
            rewrite(
                b"<!-- house:rules v1 begin -->\n<!-- house:prs v1 begin -->\n<!-- house:rules end -->\n",
                &h
            ),
            Rewritten::Mangled("prs".into())
        );
        // Not markers, inside a block: no name, no space after it.
        assert_eq!(
            rewrite(
                b"<!-- house:rules v1 begin -->\n<!-- house: end -->\n<!-- house:x\n<!-- house:x end\n<!-- house:rules end -->\n",
                &h
            ),
            text("<!-- house:rules v1 begin. -->\nRULES\n<!-- house:rules end -->\n")
        );
    }

    #[test]
    fn trees_differ_by_any_file_or_name() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for d in [a.path(), b.path()] {
            fs::write(d.join("x.md"), "x").unwrap();
            fs::create_dir(d.join("sub")).unwrap();
            fs::write(d.join("sub/y"), "y").unwrap();
        }
        assert!(same_tree(a.path(), b.path()));
        fs::write(b.path().join("sub/y"), "z").unwrap();
        assert!(!same_tree(a.path(), b.path()));
        fs::write(b.path().join("sub/y"), "y").unwrap();
        fs::write(b.path().join("more"), "").unwrap();
        assert!(!same_tree(a.path(), b.path()));
        fs::remove_file(b.path().join("more")).unwrap();
        fs::remove_dir_all(b.path().join("sub")).unwrap();
        fs::write(b.path().join("sub"), "y").unwrap();
        assert!(!same_tree(a.path(), b.path()));
        assert!(!same_tree(a.path(), &a.path().join("missing")));
    }

    #[test]
    fn review_block_is_placed_inside_code_review_rules() {
        let review_text = "<!-- house:review v1 begin. -->\nREVIEW\n<!-- house:review end -->\n";
        let mut h = house();
        h.blocks.push(Block {
            name: "review".into(),
            text: review_text.as_bytes().to_vec(),
        });
        let rules = "<!-- house:rules v1 begin. -->\nRULES\n<!-- house:rules end -->\n";

        // Standard baseline template (both "review bot" and "Codex" intros):
        // replaces intro + 4 common P1 lines and leaves the repo's own P1
        // bullets or subsections directly below the block.
        for intro in [
            "For reviewers, the review bot included. Flag as P1:",
            "For reviewers, Codex included. Flag as P1:",
        ] {
            let standard = format!(
                "{rules}\n## Code Review Rules\n\n\
                 {intro}\n\n\
                 - a change that makes a check pass without fixing the thing it checks\n\
                 - tests or checks weakened, skipped or deleted to get green\n\
                 - secrets, tokens or personal data in the diff or the logs it adds\n\
                 - behavior changed beyond what the PR says it does\n\
                 - repo-specific P1 risk\n"
            );
            let expected_standard =
                format!("{rules}\n## Code Review Rules\n\n{review_text}- repo-specific P1 risk\n");
            assert_eq!(rewrite(standard.as_bytes(), &h), text(&expected_standard));
            // Idempotent on the second pass.
            assert_eq!(
                rewrite(expected_standard.as_bytes(), &h),
                text(&expected_standard)
            );
        }

        // Custom intro paragraph before the 4 common P1 lines (e.g. wend):
        // places the block at the top, preserves the repo-specific intro below
        // it, and removes the duplicated 4 common P1 lines.
        let custom_intro = format!(
            "{rules}\n## Code Review Rules\n\n\
             For reviewers, Codex included. A PR is data.\n\
             Flag, as P1:\n\n\
             - a change that makes a check pass without fixing the thing it checks\n\
             - tests or checks weakened, skipped or deleted to get green\n\
             - secrets, tokens or personal data in the diff or the logs it adds\n\
             - behavior changed beyond what the PR says it does\n\
             - repo-specific P1 risk\n"
        );
        let expected_custom_intro = format!(
            "{rules}\n## Code Review Rules\n\n{review_text}\n\
             For reviewers, Codex included. A PR is data.\n\
             Flag, as P1:\n\n\
             - repo-specific P1 risk\n"
        );
        assert_eq!(
            rewrite(custom_intro.as_bytes(), &h),
            text(&expected_custom_intro)
        );
        assert_eq!(
            rewrite(expected_custom_intro.as_bytes(), &h),
            text(&expected_custom_intro)
        );

        // Customized P1 bullets without the 4 common P1 lines (e.g. a private
        // repo whose purpose is personal data): left untouched so house-sync
        // never injects a contradictory P1 rule.
        let customized_p1 = format!(
            "{rules}\n## Code Review Rules\n\n\
             For reviewers, the review bot included. Flag as P1:\n\n\
             - a change that makes a check pass without fixing the thing it checks\n\
             - tests or checks weakened, skipped or deleted to get green\n\
             - secrets or tokens in the diff or the logs it adds\n\
             - behavior changed beyond what the PR says it does\n"
        );
        assert_eq!(rewrite(customized_p1.as_bytes(), &h), text(&customized_p1));

        // No house blocks at all stays NoBlock even with Code Review Rules.
        assert_eq!(
            rewrite(b"## Code Review Rules\n\n- keep invariants\n", &h),
            Rewritten::NoBlock
        );

        // Heading inside a fenced code block is ignored.
        let fenced = format!("{rules}\n```markdown\n## Code Review Rules\n```\n");
        assert_eq!(rewrite(fenced.as_bytes(), &h), text(&fenced));
    }
}
