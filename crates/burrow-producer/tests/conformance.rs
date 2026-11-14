//! A kit plugin passes Conary's own `cn plugin check` (`Plugin::check`):
//! the runner describes it, sends it every fixture of the kinds it serves
//! at the contract they agree on, and holds each answer to the execution it
//! asked for and the offers it was made. Conary runs this binary as the
//! plugin: by its name, a model plugin's or a program's.
use burrow_producer::plugin::Plugin;
use conary_core::plugin::{
    Answer, Capabilities, Identity, Kind, Limits, Model, Plugin as Runner, Sandbox, Task,
};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

const MODEL: &str = "model-plugin";
const PROGRAM: &str = "program-plugin";

fn main() -> ExitCode {
    let me = std::env::args_os().next().unwrap_or_default();
    let name = Path::new(&me).file_name().unwrap_or_default().to_owned();
    if name == MODEL || name == PROGRAM {
        return plugin(name == MODEL);
    }
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap();
    for name in [MODEL, PROGRAM] {
        let path = dir.path().join(name);
        std::os::unix::fs::symlink(&exe, &path).unwrap();
        let limits = Limits {
            deadline: Duration::from_secs(30),
            response_bytes: 1 << 20,
        };
        let runner = Runner::describe(&path, limits)
            .unwrap_or_else(|e| panic!("{name} doesn't describe: {e:?}"));
        let checked = runner
            .check()
            .unwrap_or_else(|e| panic!("{name} fails Conary's check: {e:?}"));
        assert_eq!(checked, [Kind::Review], "{name}");
        println!("{name}: passes Conary's plugin check");
    }
    ExitCode::SUCCESS
}

/// Answer as a kit plugin that reviews, with or without a model.
fn plugin(model: bool) -> ExitCode {
    let models = if model {
        vec![Model {
            id: "a-model".into(),
            efforts: vec!["high".into()],
            context_tokens: None,
            max_output_tokens: None,
        }]
    } else {
        vec![]
    };
    let kit = Plugin {
        describe: Answer::Describe {
            versions: vec![conary_core::plugin::CONTRACT.into()],
            kinds: vec![Kind::Review],
            v2_kinds: vec![],
            v4_kinds: vec![],
            models,
            capabilities: Some(Capabilities {
                sandbox: Sandbox::Brings,
                network: false,
                devices: vec![],
                mcp: false,
            }),
            meters: vec![],
        },
        by: Identity {
            engine: "kit-conformance".into(),
            session: None,
        },
        fixture: |task| {
            matches!(task, Task::Review { .. }).then(|| Answer::Review { findings: vec![] })
        },
        socket: "/nonexistent/producer.sock".into(),
    };
    match kit.answer(&mut std::io::stdin(), &mut std::io::stdout()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
