//! The house blocks' facts: in the repo that owns them (burrow, with its
//! house/), its own AGENTS.md carries each block as house/ has it,
//! verbatim, markers and all. Every other repo's copy is house-sync's to
//! keep, so a tree with no house/ is held to none of these.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use house_sync::blocks::House;

use crate::{Finding, Fix};

/// Where the house blocks and their copy are, from the repo's root.
const HOUSE: &str = "house";
const AGENTS: &str = "AGENTS.md";

/// The findings in the tree under `root`: a house block AGENTS.md lacks,
/// or carries other than as house/ has it.
pub fn check(root: &Path) -> Result<Vec<Finding>, String> {
    let dir = root.join(HOUSE);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let house = House::read(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = root.join(AGENTS);
    let agents = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    Ok(house
        .blocks
        .iter()
        .filter(|b| !agents.contains(&*String::from_utf8_lossy(&b.text)))
        .map(|b| {
            let begin = format!("<!-- house:{} ", b.name);
            let at = agents
                .lines()
                .position(|l| l.trim_start().starts_with(&begin));
            Finding {
                file: AGENTS.to_owned(),
                line: at.map_or(1, |i| i + 1),
                what: format!(
                    "the house block `{}` {}",
                    b.name,
                    if at.is_some() { "changed" } else { "nowhere" }
                ),
                owner: format!("{HOUSE}/{}.md", b.name),
                fix: Fix::Copy,
            }
        })
        .collect())
}
