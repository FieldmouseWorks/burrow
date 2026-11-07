//! Guide size: an AGENTS.md is a map. A PR that leaves one's own part, every
//! line outside the house blocks, longer than it found it and past GUIDE
//! lines at its head says why. The house blocks never count.

use burrow_house::rules::GUIDE;

use crate::*;

const REASON: &str = "Why a longer guide: the contributor walkthrough is the product here.";

fn own(n: usize) -> String {
    "x\n".repeat(n)
}

fn blocks(n: usize) -> String {
    format!(
        "<!-- house:rules v1 begin -->\n{}<!-- house:rules end -->\n",
        "r\n".repeat(n)
    )
}

#[test]
fn grown() {
    for c in [
        case(vec![change("modified", "AGENTS.md", 5, 0)])
            .tree("4ead", "AGENTS.md", &own(GUIDE + 1))
            .tree("3e7b", "AGENTS.md", &own(GUIDE - 4)),
        // Already past it, and grown further.
        case(vec![change("modified", "AGENTS.md", 2, 1)])
            .tree("4ead", "AGENTS.md", &own(GUIDE + 20))
            .tree("3e7b", "AGENTS.md", &own(GUIDE + 19)),
        // A new one, wherever it is.
        case(vec![change("added", "web/AGENTS.md", GUIDE as u64 + 1, 0)]).tree(
            "4ead",
            "web/AGENTS.md",
            &own(GUIDE + 1),
        ),
        case(vec![from(
            change("copied", "b/AGENTS.md", 0, 0),
            "a/AGENTS.md",
        )])
        .tree("4ead", "b/AGENTS.md", &own(GUIDE + 1)),
        // Something else renamed to one is new to it, lines added or not.
        case(vec![from(
            change("renamed", "AGENTS.md", 0, 0),
            "docs/guide.md",
        )])
        .tree("4ead", "AGENTS.md", &own(GUIDE + 1)),
        // Its own part grew while the whole file shrank: the house blocks
        // lost more lines than the guide gained.
        case(vec![change("modified", "AGENTS.md", 1, 10)])
            .tree(
                "4ead",
                "AGENTS.md",
                &format!("{}{}", own(GUIDE + 1), blocks(5)),
            )
            .tree(
                "3e7b",
                "AGENTS.md",
                &format!("{}{}", own(GUIDE), blocks(15)),
            ),
        case(vec![from(
            change("renamed", "AGENTS.md", 1, 0),
            "docs/guide.md",
        )])
        .tree("4ead", "AGENTS.md", &own(GUIDE + 1)),
    ] {
        fails(&c, "guide-size");
        passes(&c.clone().body(REASON));
    }
}

// Lines inside the house blocks are the house's, so a sync that grows them,
// or a guide whose blocks take it past the limit, passes; so does one at the
// limit, one shrunk, and a file only named like one.
#[test]
fn not_grown() {
    let mut big = own(GUIDE);
    big.push_str(&blocks(200));
    passes(
        &case(vec![change("modified", "AGENTS.md", 150, 0)])
            .tree("4ead", "AGENTS.md", &big)
            .tree(
                "3e7b",
                "AGENTS.md",
                &format!("{}{}", own(GUIDE), blocks(50)),
            ),
    );
    passes(
        &case(vec![change("modified", "AGENTS.md", 1, 5)])
            .tree("4ead", "AGENTS.md", &own(GUIDE + 10))
            .tree("3e7b", "AGENTS.md", &own(GUIDE + 14)),
    );
    passes(
        &case(vec![change("modified", "AGENTS.md", 2, 1)])
            .tree("4ead", "AGENTS.md", &own(GUIDE))
            .tree("3e7b", "AGENTS.md", &own(GUIDE - 1)),
    );
    passes(&case(vec![change("modified", "docs/MY-AGENTS.md", 200, 0)]));
    passes(&case(vec![change("removed", "AGENTS.md", 0, 300)]));
}

// No answer for its content is no verdict.
#[test]
fn no_answer() {
    let o = case(vec![change("modified", "AGENTS.md", 20, 5)])
        .body(REASON)
        .tree("3e7b", "AGENTS.md", &own(10))
        .unread("4ead", "AGENTS.md")
        .run();
    assert!(o.result.is_err() && o.posted.is_empty(), "{}", o.out);
}
