//! Checks for burrow's packaged guidance. The PR house action still owns
//! guide growth and its reason; this checks the committed source and copies.

use std::fs;
use std::path::{Path, PathBuf};

use house_sync::blocks::{House, Rewritten, rewrite};

/// Root guides and Markdown below docs/ and house/, the caller's docs-paths.
/// Missing root guides and the house directory are errors, not empty scans.
pub fn files(root: &Path) -> Result<Vec<PathBuf>, String> {
    fn markdown(dir: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() {
                markdown(&path, paths)?;
            } else if path.extension().is_some_and(|e| e == "md") {
                paths.push(path);
            }
        }
        Ok(())
    }
    let mut paths: Vec<_> = ["README.md", "AGENTS.md", "CLAUDE.md"]
        .iter()
        .map(|p| root.join(p))
        .collect();
    markdown(&root.join("docs"), &mut paths)?;
    markdown(&root.join("house"), &mut paths)?;
    paths.sort();
    Ok(paths)
}

pub fn check(root: &Path) -> Result<(), String> {
    let house = House::read(&root.join("house")).map_err(|e| e.to_string())?;
    for block in &house.blocks {
        let prefix = format!("<!-- house:{} v", block.name);
        let suffix = format!("<!-- house:{} end -->\n", block.name);
        if !block.text.starts_with(prefix.as_bytes())
            || !block.text.ends_with(suffix.as_bytes())
            || rewrite(&block.text, &house) != Rewritten::Text(block.text.clone())
        {
            return Err(format!("house/{}.md: invalid house block", block.name));
        }
    }
    let agents = fs::read(root.join("AGENTS.md")).map_err(|e| e.to_string())?;
    // Check every source individually: rewrite intentionally permits a repo
    // without a block, whereas burrow must carry every block it publishes.
    for block in &house.blocks {
        if !agents.windows(block.text.len()).any(|w| w == block.text) {
            return Err(format!(
                "AGENTS.md: missing verbatim house:{} block",
                block.name
            ));
        }
    }
    let marker_lines = agents
        .split(|&b| b == b'\n')
        .filter(|line| line.starts_with(b"<!-- house:"))
        .count();
    if marker_lines != house.blocks.len() * 2 {
        return Err("AGENTS.md: house markers without matching source blocks".into());
    }
    if rewrite(&agents, &house) != Rewritten::Text(agents) {
        return Err("AGENTS.md: invalid or inconsistent house blocks".into());
    }
    let claude = fs::read_to_string(root.join("CLAUDE.md")).map_err(|e| e.to_string())?;
    if claude.lines().next() != Some("@AGENTS.md") {
        return Err("CLAUDE.md: first line must import @AGENTS.md".into());
    }
    for path in files(root)? {
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !text.ends_with('\n') || text.contains('\r') || text.contains('\0') {
            return Err(format!(
                "{}: expected UTF-8 text with LF lines and a final newline",
                path.display()
            ));
        }
        for (line, text) in text.lines().enumerate() {
            if text.starts_with("<<<<<<< ") || text == "=======" || text.starts_with(">>>>>>> ") {
                return Err(format!(
                    "{}:{}: merge conflict marker",
                    path.display(),
                    line + 1
                ));
            }
        }
    }
    Ok(())
}
