//! The fixture around another plugin answers as that plugin: its engine, and
//! for a plugin that offers models, the first one it offers, so Conary's
//! execution check accepts the answer.

use burrow_example_plugin::around::as_plugin;
use conary_core::plugin::{
    Answer, CONTRACT, CONTRACT_V1, Capabilities, Context, Executed, Execution, Identity, Kind,
    Model, Negotiated, Next, Offers, Provenance, Response, Sandbox,
};
use std::path::{Path, PathBuf};
use std::process::Command;

fn capabilities() -> Capabilities {
    Capabilities {
        sandbox: Sandbox::Needs,
        network: true,
        devices: vec![],
        mcp: false,
    }
}

fn described_with(models: Vec<Model>) -> Response {
    Response {
        contract: CONTRACT_V1.to_owned(),
        by: Identity {
            engine: "fixture-engine".to_owned(),
            session: None,
        },
        execution: None,
        answer: Answer::Describe {
            versions: vec![CONTRACT_V1.to_owned(), CONTRACT.to_owned()],
            kinds: vec![],
            v2_kinds: vec![Kind::Job],
            v4_kinds: vec![],
            models,
            capabilities: Some(capabilities()),
            meters: vec![],
        },
    }
}

fn job_response() -> Response {
    Response {
        contract: CONTRACT.to_owned(),
        by: Identity {
            engine: "example".to_owned(),
            session: None,
        },
        execution: Some(Executed {
            model: None,
            effort: None,
            context: Context::Fresh,
        }),
        answer: Answer::Job {
            summary: "ok".to_owned(),
            next: Next::Done,
        },
    }
}

/// A plugin that answers every request with `described`, installed by a
/// child so no writable descriptor of it is ever open in this process.
fn plugin(dir: &Path, name: &str, described: &Response) -> PathBuf {
    let answer = dir.join(format!("{name}.json"));
    std::fs::write(&answer, serde_json::to_vec(described).unwrap()).unwrap();
    let source = dir.join(format!("{name}.sh"));
    std::fs::write(
        &source,
        format!("#!/bin/sh\ncat >/dev/null\ncat '{}'\n", answer.display()),
    )
    .unwrap();
    let path = dir.join(name);
    let installed = Command::new("install")
        .args(["-m", "0755", "--"])
        .arg(&source)
        .arg(&path)
        .status()
        .unwrap();
    assert!(installed.success(), "install {}", path.display());
    path
}

fn dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("around-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn around_a_model_plugin_it_answers_with_the_first_offered_model() {
    let models = vec![
        Model {
            id: "offered-model".to_owned(),
            efforts: vec!["low".to_owned(), "high".to_owned()],
            context_tokens: None,
            max_output_tokens: None,
        },
        Model {
            id: "second-model".to_owned(),
            efforts: vec![],
            context_tokens: None,
            max_output_tokens: None,
        },
    ];
    let described = described_with(models.clone());
    let dir = dir("model");
    let wrapped = plugin(&dir, "model-plugin", &described);
    let answered = as_plugin(&wrapped, job_response()).unwrap();
    assert_eq!(answered.by.engine, "fixture-engine");
    assert_eq!(
        answered.execution.as_ref().and_then(|e| e.model.as_deref()),
        Some("offered-model")
    );

    // What Conary checks the answer against: the plugin's own describe.
    let negotiated = Negotiated {
        by: described.by,
        offers: Offers {
            models,
            capabilities: capabilities(),
            meters: vec![],
        },
    };
    let requested = Execution {
        choice: None,
        context: Context::Fresh,
    };
    let provenance = Provenance {
        by: answered.by,
        execution: answered.execution.unwrap(),
    };
    negotiated.verify(&requested, &provenance).unwrap();
}

#[test]
fn around_a_program_plugin_it_names_no_model() {
    let dir = dir("program");
    let wrapped = plugin(&dir, "program-plugin", &described_with(vec![]));
    let answered = as_plugin(&wrapped, job_response()).unwrap();
    assert_eq!(answered.by.engine, "fixture-engine");
    assert_eq!(
        answered.execution.as_ref().and_then(|e| e.model.as_deref()),
        None
    );
}
