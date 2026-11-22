//! The caller for the sandbox's checks: Conary's own runner, on an
//! executable as it's given one.
//!
//! ```text
//! burrow-example-plugin-call check PLUGIN
//! burrow-example-plugin-call job PLUGIN WORKDIR LOOK
//! ```
//!
//! `job` prints the plugin's report.

use conary_core::plugin::{Answer, Limits, Plugin, Task};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

fn run(args: &[String]) -> Result<String, String> {
    let limits = Limits {
        deadline: Duration::from_secs(2400),
        response_bytes: 1_048_576,
    };
    let described = |path: &str| Plugin::describe(Path::new(path), limits).map_err(|e| e.0);
    match args {
        [word, path] if word == "check" => {
            let checked = described(path)?.check().map_err(|e| e.0)?;
            serde_json::to_string(&checked).map_err(|e| e.to_string())
        }
        [word, path, workdir, look] if word == "job" => {
            let task = Task::Job {
                repo: "o/demo".to_owned(),
                workdir: workdir.clone(),
                prompt: look.clone(),
                model: None,
                effort: None,
            };
            match described(path)?.call(task).map_err(|e| e.0)?.answer {
                Answer::Job { summary, .. } => Ok(summary),
                _ => Err("not a job's answer".to_owned()),
            }
        }
        _ => Err("usage: check PLUGIN | job PLUGIN WORKDIR LOOK".to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(said) => {
            println!("{said}");
            ExitCode::SUCCESS
        }
        Err(why) => {
            eprintln!("burrow-example-plugin-call: {why}");
            ExitCode::FAILURE
        }
    }
}
