//! Model names: a line the PR adds that names a model, Markdown and comments
//! included, since guidance is where they creep in. Case matters, so a word
//! that only looks like one passes.

use crate::*;

const REASON: &str = "Why a model name: the test replays the CLI's own banner.";

fn adding(path: &str, line: &str) -> Vec<serde_json::Value> {
    vec![patched(
        change("modified", path, 1, 0),
        adds(&[line], &[], &[]),
    )]
}

#[test]
fn names() {
    for line in [
        "Route the reviewer to Opus 5.5 at high effort.",
        "model = \"claude-sonnet-4-5\"",
        "  WEND_MODEL=anthropic:claude-opus-5-5",
        "# Haiku 4 handles the triage",
        "an older claude-3-opus run",
        "A gpt-5 run.",
        "GPT-4o wrote it",
        "uses Gemini 2.5",
        "gemini-2.5-pro",
        "llama3 locally",
        "Qwen3-coder",
        "DeepSeek-V3 said so",
        "o3-mini",
        "Fable 5 thinks",
    ] {
        fails(&case(adding("AGENTS.md", line)), "model-names");
        passes(&case(adding("AGENTS.md", line)).body(REASON));
        passes(&case(vec![patched(
            change("modified", "AGENTS.md", 0, 1),
            adds(&[], &[], &[line]),
        )]));
    }
}

#[test]
fn look_alikes() {
    for line in [
        "an opus of a function",
        "Egypt and gptables",
        "the Claude Code CLI",
        "a haiku 4 lines long",
        "sonnet 18",
        "claude.ai/code",
        "openai:",
        "proto3-mini",
        "fable-5",
        "omega-3",
    ] {
        passes(&case(adding("docs/x.md", line)));
    }
}

// A file GitHub gives no patch for is read whole, Markdown included.
#[test]
fn no_patch() {
    let c = case(vec![unpatched(change("modified", "AGENTS.md", 1, 0))]).tree(
        "3e7b",
        "AGENTS.md",
        "# Agents\n",
    );
    fails(
        &c.clone()
            .tree("4ead", "AGENTS.md", "# Agents\nRoute it to Opus 5.5.\n"),
        "model-names",
    );
    passes(&c.tree("4ead", "AGENTS.md", "# Agents\nRoute it to the reviewer.\n"));
    // A copy is read whole, whatever its patch shows.
    let copy = case(vec![from(change("copied", "docs/b.md", 0, 0), "docs/a.md")]);
    fails(
        &copy
            .clone()
            .tree("4ead", "docs/b.md", "Route it to Opus 5.5.\n"),
        "model-names",
    );
    passes(&copy.tree("4ead", "docs/b.md", "Route it to the reviewer.\n"));
}

// A repo whose subject is models turns the rule off: it posts no status for
// it.
#[test]
fn off() {
    let mut c = case(adding("fixes/x.toml", "author = \"claude-opus-5-5\""));
    c.model_names = false;
    let o = passes(&c);
    assert!(
        !o.posted
            .iter()
            .any(|p| p.status.context.contains("model-names"))
    );
}

#[test]
fn fork() {
    let mut c = case(adding("x.md", "Opus 5"));
    c.fork = Fork::Yes;
    fails(&c, "model-names");
}
