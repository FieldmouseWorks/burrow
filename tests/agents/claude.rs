//! Claude's stream protocol for the agents VM test, without a login.
use std::{
    io::{self, Read},
    process::{Command, ExitCode, Stdio},
};

fn declaration(prompt: &str) -> Option<Vec<&str>> {
    let prompt = prompt.trim_end();
    if prompt.starts_with("silent") {
        None
    } else if prompt.starts_with("Your answer didn't say what comes next") {
        // Nudged, it awaits the issue the test serves, so one turn shows
        // the nudge and a declaration that GitHub is read for.
        Some(vec!["await", "https://github.com/o/r/issues/1"])
    } else {
        Some(vec!["done"])
    }
}

fn main() -> ExitCode {
    // agent's pre-job smoke probe has no PROJ_VM_NEXT and must not touch
    // stdin or try to declare a job step.
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("Claude test fixture");
        return ExitCode::SUCCESS;
    }
    let mut prompt = String::new();
    io::stdin().read_to_string(&mut prompt).unwrap();
    let mut result = "answered".to_owned();
    if prompt.trim() == "probe" {
        let answer = Command::new("proj").args(["probe", "deployed"]).output().unwrap();
        if !answer.status.success() { return ExitCode::FAILURE; }
        result = String::from_utf8(answer.stdout).unwrap();
    }
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
    let result = result.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n");
    println!(r#"{{"type":"result","session_id":"vm-test","result":"{result}","is_error":false}}"#);
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
            Some(vec!["await", "https://github.com/o/r/issues/1"])
        );
        assert_eq!(
            declaration("News on what you were waiting on"),
            Some(vec!["done"])
        );
    }
}
