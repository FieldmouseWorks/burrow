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

/// AGENTS.md as it should be. A block it doesn't have stays out.
pub fn rewrite(agents: &[u8], house: &House) -> Rewritten {
    let mut text = agents.to_vec();
    let mut found = false;
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
    }
    if found {
        Rewritten::Text(text)
    } else {
        Rewritten::NoBlock
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

/// Any block's begin or end marker.
fn marker(line: &[u8]) -> bool {
    let Some(rest) = line.strip_prefix(b"<!-- house:") else {
        return false;
    };
    match rest.iter().position(|&b| b == b' ') {
        Some(space) if space > 0 => {
            let after = &rest[space + 1..];
            after.starts_with(b"end -->") || version_begin(after)
        }
        _ => false,
    }
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
}
