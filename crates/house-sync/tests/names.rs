//! The house blocks are copied into every repo's AGENTS.md, and a repo may
//! hold its guidance to naming roles (consolebook's guard fails on these
//! words), so a block that names a vendor or a model turns that repo's sync
//! PR red. The blocks say "job" and "review bot", whichever tool runs them.

use std::fs;
use std::path::Path;

const NAMES: &[&str] = &[
    "gpt",
    "chatgpt",
    "openai",
    "codex",
    "claude",
    "anthropic",
    "opus",
    "sonnet",
    "haiku",
    "fable",
    "gemini",
    "copilot",
    "deepseek",
    "llama",
    "mistral",
    "grok",
    "qwen",
];

/// The names in `text`, the way those guards match them: any case, no letter
/// or digit before it, no letter after it.
fn named(text: &str) -> Vec<&'static str> {
    let text = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    NAMES
        .iter()
        .copied()
        .filter(|name| {
            text.match_indices(name).any(|(at, _)| {
                let before = at.checked_sub(1).map(|i| bytes[i]);
                let after = bytes.get(at + name.len());
                !before.is_some_and(|b| b.is_ascii_alphanumeric())
                    && !after.is_some_and(u8::is_ascii_alphabetic)
            })
        })
        .collect()
}

#[test]
fn the_match_finds_a_name_and_leaves_longer_words() {
    assert_eq!(
        named("a\nClaude job takes it, as gpt-5 did"),
        ["gpt", "claude"]
    );
    assert!(named("an affable job, in claudes and opuses").is_empty());
}

#[test]
fn no_house_block_names_a_vendor_or_a_model() {
    let house = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../house");
    let mut read = 0;
    for entry in fs::read_dir(&house).unwrap() {
        let path = entry.unwrap().path();
        let found = named(&fs::read_to_string(&path).unwrap());
        assert!(found.is_empty(), "{} names {found:?}", path.display());
        read += 1;
    }
    assert!(read > 0, "no blocks in {}", house.display());
}
