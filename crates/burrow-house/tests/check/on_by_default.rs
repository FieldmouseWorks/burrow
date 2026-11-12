//! On by default: a line the PR adds that turns an automated spender on,
//! outside Markdown and comments.

use crate::*;

const REASON: &str = "Why on by default: the drain is this repo's whole job.";

fn adding(path: &str, line: &str) -> Vec<serde_json::Value> {
    vec![patched(
        change("modified", path, 1, 0),
        adds(&[line], &[], &[]),
    )]
}

/// The PR adds `line` to a head file that holds `file` (the line is its
/// first, as the patch says).
fn within(path: &str, line: &str, file: &str) -> Case {
    case(adding(path, line)).tree("4ead", path, file)
}

#[test]
fn spenders() {
    for (path, line) in [
        (
            "hosts/x.nix",
            "  burrow.proj.inbox.triage.drain.enable = true;",
        ),
        ("hosts/x.nix", "  extraUsage = lib.mkDefault true;"),
        ("config.toml", "paid_usage = true"),
        (".github/x.yml", "drain: true"),
        (
            "hosts/x.nix",
            "  burrow.proj.inbox.triage.drain = \"codex\";",
        ),
        ("hosts/x.nix", "  burrow.proj.usage.paid = true;"),
        (
            "hosts/x.nix",
            "  fresh = { enable = true; owner = \"o\"; };",
        ),
        ("hosts/x.nix", "  /* opt in */ fresh.enable = true;"),
        (
            "hosts/x.nix",
            "burrow.dispatch = {} // { fresh.enable = true; };",
        ),
        ("config.toml", "o = { label = '#', paid_usage = true }"),
    ] {
        fails(&within(path, line, line), "on-by-default");
        passes(&within(path, line, line).body(REASON));
    }
}

#[test]
fn not_spenders() {
    for (path, line) in [
        ("hosts/x.nix", "  drain.enable = false;"),
        ("hosts/x.nix", "  # drain.enable = true;"),
        ("hosts/x.nix", "  drain = null;"),
        ("hosts/x.nix", "  usage.paid = false;"),
        (
            "hosts/x.nix",
            "  fresh.enable = false; # was fresh.enable = true;",
        ),
        ("docs/x.md", "Set `drain.enable = true` to turn it on."),
    ] {
        passes(&within(path, line, line));
    }
}

// A line added between unchanged block-comment delimiters is inside the
// comment, whichever way GitHub gave the diff; code after the closing
// delimiter is not.
#[test]
fn block_comments_across_lines() {
    let x = "hosts/x.nix";
    let line = "fresh.enable = true;";
    let patch = "@@ -1,2 +1,3 @@\n /*\n+fresh.enable = true;\n */";
    let in_comment = format!("/*\n{line}\n*/\n");
    passes(
        &case(vec![patched(change("modified", x, 1, 0), patch.into())]).tree(
            "4ead",
            x,
            &in_comment,
        ),
    );
    // The same, as a file GitHub gives no patch for.
    passes(
        &case(vec![unpatched(change("modified", x, 1, 0))])
            .tree("4ead", x, &in_comment)
            .tree("3e7b", x, "/*\n*/\n"),
    );
    // Code after the closing delimiter is live.
    let live = format!("/*\nnote\n*/ {line}\n");
    let patch = "@@ -1,2 +1,3 @@\n /*\n note\n+*/ fresh.enable = true;";
    fails(
        &case(vec![patched(change("modified", x, 1, 0), patch.into())]).tree("4ead", x, &live),
        "on-by-default",
    );
    fails(
        &case(vec![unpatched(change("modified", x, 1, 0))])
            .tree("4ead", x, &live)
            .tree("3e7b", x, "/*\nnote\n"),
        "on-by-default",
    );
}
