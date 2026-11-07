//! An AGENTS.md's own lines: everything but the house blocks, whose markers
//! are the blocks' too.

use house_sync::blocks::own_lines;

#[test]
fn blocks_are_not_the_repos() {
    let agents = "# Title\n\nIntro.\n\
        <!-- house:rules v1 begin. x -->\n## House rules\n- one\n<!-- house:rules end -->\n\
        ## Mine\n\
        <!-- house:prs v12 begin -->\nPRS\n<!-- house:prs end -->\n\
        ## Review";
    assert_eq!(own_lines(agents.as_bytes()), 5);
}

#[test]
fn no_blocks() {
    assert_eq!(own_lines(b""), 0);
    assert_eq!(own_lines(b"a\nb\n"), 2);
    assert_eq!(own_lines(b"a\nb"), 2);
}

// Only a whole block is the house's: a begin with no end, or what only looks
// like a marker, stays the repo's.
#[test]
fn not_a_block() {
    assert_eq!(own_lines(b"a\n<!-- house:rules v1 begin -->\nb\nc\n"), 4);
    assert_eq!(
        own_lines(b"<!-- house:rules begin -->\nb\n<!-- house:rules end -->\n"),
        3
    );
    assert_eq!(
        own_lines(b"<!-- house: v1 begin -->\nb\n<!-- house:rules end -->\n"),
        3
    );
}

// A block's end is its own name's: an unclosed rules block can't borrow the
// prs block's end, and a nested or interleaved marker breaks the span.
#[test]
fn names_must_match() {
    let own = "x\n".repeat(101);
    let agents = format!(
        "<!-- house:rules v1 begin -->\n{own}<!-- house:prs v1 begin -->\nP\n<!-- house:prs end -->\n"
    );
    assert_eq!(own_lines(agents.as_bytes()), 102);
    assert_eq!(
        own_lines(b"<!-- house:rules v1 begin -->\na\n<!-- house:prs end -->\n"),
        3
    );
    assert_eq!(
        own_lines(
            b"<!-- house:rules v1 begin -->\n<!-- house:prs v1 begin -->\nP\n<!-- house:prs end -->\n<!-- house:rules end -->\n"
        ),
        2
    );
}
