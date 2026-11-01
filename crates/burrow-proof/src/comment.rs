//! What a run says on its PR (`comment.md`), the steward posting it as it
//! is: for each check, what its command wrote to PROOF_SUMMARY, and for a
//! failure that wrote none, its exit and where its log is. Never the log
//! itself, which stays on the machine: a command's output can hold what
//! its repo keeps private, and the summary is the command's own word on
//! what's fit to post.

use std::fs;
use std::path::Path;

use crate::{Ran, Record, summary};

/// The comment for RAN, the run whose folder is DIR and whose id is ID, or
/// None when nothing failed and nothing was written. It fits in the
/// record's commentBytes, each summary cut to its share of the room the
/// headings leave.
pub fn comment(rec: &Record, dir: &Path, id: &str, ran: &Ran) -> Option<String> {
    let mut parts = Vec::new();
    for (i, check) in rec.runs.keys().enumerate() {
        let Some([state, _]) = ran.verdicts.get(check) else {
            continue;
        };
        let said = fs::read(summary(dir, i))
            .map(|b| String::from_utf8_lossy(&b).trim().to_owned())
            .unwrap_or_default();
        let head = if state == "failure" {
            let exit = ran.exits.get(check).copied().unwrap_or_default();
            let mut h = format!(
                "`{check}` failed on {}: it ran here and exited {exit}.",
                rec.head
            );
            if said.is_empty() {
                h.push_str(&format!(
                    " What it printed stays on this machine, in the log of run `{id}`."
                ));
            }
            h
        } else if !said.is_empty() {
            format!("`{check}` passed on {}.", rec.head)
        } else {
            continue;
        };
        parts.push((head, said));
    }
    if parts.is_empty() {
        return None;
    }
    let headings: usize = parts.iter().map(|(h, _)| h.len() + 4).sum();
    let room = rec.comment_bytes.saturating_sub(headings + 1) / parts.len();
    let out: Vec<String> = parts
        .into_iter()
        .map(|(head, said)| {
            if said.is_empty() {
                head
            } else {
                format!("{head}\n\n{}", cut(&said, room))
            }
        })
        .collect();
    Some(out.join("\n\n") + "\n")
}

const MORE: &str = "\n…";

/// TEXT in at most ROOM bytes: its start, and a mark where it was cut, at a
/// character's edge.
pub fn cut(text: &str, room: usize) -> String {
    if text.len() <= room {
        return text.to_owned();
    }
    if room < MORE.len() {
        return String::new();
    }
    let mut end = room - MORE.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{MORE}", &text[..end])
}
