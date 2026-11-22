//! The caller for the sandbox's checks: Conary's own runner, on an
//! executable as it's given one.
//!
//! ```text
//! burrow-example-plugin-call check PLUGIN
//! burrow-example-plugin-call job PLUGIN WORKDIR LOOK
//! burrow-example-plugin-call holds PLUGIN REPORT...
//! ```
//!
//! `check` is `cn plugin check`'s own. `job` prints the plugin's report.
//! `holds` fails, saying why, unless each report (a file `job` printed, for
//! the fixture around PLUGIN) shows what PLUGIN's describe declares working
//! and nothing else (burrow_example_plugin::holds).

use burrow_example_plugin::{Report, holds};
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
        [word, path, reports @ ..] if word == "holds" && !reports.is_empty() => {
            let plugin = described(path)?;
            let offered = plugin
                .offers()
                .map(|offers| &offers.capabilities)
                .ok_or("the plugin declares no capabilities")?;
            let declared = burrow_grants::declared(plugin.kinds(), offered);
            let mut wrong = Vec::new();
            for at in reports {
                let report: Report = std::fs::read(at)
                    .map_err(|e| e.to_string())
                    .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|e| e.to_string()))
                    .map_err(|e| format!("{at}: {e}"))?;
                wrong.extend(
                    holds(&declared, &report)
                        .into_iter()
                        .map(|w| format!("{at}: {w}")),
                );
            }
            if wrong.is_empty() {
                serde_json::to_string(&declared).map_err(|e| e.to_string())
            } else {
                Err(format!(
                    "the plugin isn't held to what it declares:\n{}",
                    wrong.join("\n")
                ))
            }
        }
        _ => {
            Err("usage: check PLUGIN | job PLUGIN WORKDIR LOOK | holds PLUGIN REPORT...".to_owned())
        }
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
