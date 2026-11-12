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
        // Comments aren't parsed, so a commented one asks for a reason too.
        ("hosts/x.nix", "  # drain.enable = true;"),
        (
            "hosts/x.nix",
            "  fresh.enable = false; # was fresh.enable = true;",
        ),
    ] {
        fails(&case(adding(path, line)), "on-by-default");
        passes(&case(adding(path, line)).body(REASON));
    }
}

#[test]
fn not_spenders() {
    for (path, line) in [
        ("hosts/x.nix", "  drain.enable = false;"),
        ("hosts/x.nix", "  drain = null;"),
        ("hosts/x.nix", "  usage.paid = false;"),
        ("docs/x.md", "Set `drain.enable = true` to turn it on."),
    ] {
        passes(&case(adding(path, line)));
    }
}
