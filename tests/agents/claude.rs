//! Claude's stream protocol for the agents VM test, without a login.
use std::{
    io::{self, Read},
    process::{Command, ExitCode, Stdio},
};

fn declaration(prompt: &str) -> Option<Vec<&str>> {
    let prompt = prompt.trim_end();
    if prompt.starts_with("silent") || prompt.starts_with("Your answer didn't say what comes next")
    {
        None
    } else if prompt == "ask" {
        Some(vec!["ask"])
    } else if let Some(url) = prompt.strip_prefix("await ") {
        Some(vec!["await", url])
    } else {
        Some(vec!["done"])
    }
}

fn main() -> ExitCode {
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt).unwrap();
    if let Some(args) = declaration(&prompt) {
        let status = Command::new("proj")
            .args(args)
            .stdout(Stdio::null())
            .status()
            .unwrap();
        if !status.success() {
            return ExitCode::FAILURE;
        }
    }
    println!(r#"{{"type":"system","subtype":"init","session_id":"vm-test"}}"#);
    println!(r#"{{"type":"result","session_id":"vm-test","result":"answered","is_error":false}}"#);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn prompt_selects_the_declaration() {
        assert_eq!(declaration("silent\n"), None);
        assert_eq!(
            declaration("Your answer didn't say what comes next. Say it now"),
            None
        );
        assert_eq!(declaration("ask\n"), Some(vec!["ask"]));
        assert_eq!(
            declaration("await https://github.com/o/r/issues/1\n"),
            Some(vec!["await", "https://github.com/o/r/issues/1"])
        );
        assert_eq!(
            declaration("News on what you were waiting on"),
            Some(vec!["done"])
        );
    }
}
