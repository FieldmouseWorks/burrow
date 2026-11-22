//! A test fixture: an engine plugin for the sandbox's checks
//! (tests/agents-plugins.nix). It runs no model.
//!
//! It declares the whole job contract. Conary's conformance job gets its
//! offline answer. Any other job's prompt is a [`Look`]: it tries every
//! capability by the way a grant renders it, whatever it was told it holds,
//! and reports what worked ([`Report`]), so a check sees what is withheld
//! as well as what is granted. It uses the root it has in its VM to try.

mod escape;
mod uses;

use conary_core::plugin::{
    Answer, CONTRACTS, Capabilities, Executed, Identity, Kind, Next, Request, Response, Sandbox,
    Task, fixtures,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The variable that has the fixture declare a sandbox of its own.
pub const BRINGS: &str = "EXAMPLE_PLUGIN_BRINGS_SANDBOX";

/// What a job asks it to try.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Look {
    /// A host that the engine's allowlist has, and what it answers.
    pub allowed: Option<(String, String)>,
    /// Hosts to ask the proxy for.
    pub hosts: Vec<String>,
    /// Addresses to dial around the proxy.
    pub tcp: Vec<String>,
    /// Addresses to send a datagram to and wait for one back.
    pub udp: Vec<String>,
    /// vsock ports of the host's to dial.
    pub vsock: Vec<u32>,
    /// Texts to search the whole VM for, as root.
    pub secrets: Vec<String>,
    /// Paths to read, as root.
    pub read: Vec<PathBuf>,
    /// Paths to write, as root.
    pub write: Vec<PathBuf>,
    /// virtiofs tags to mount, as root.
    pub tags: Vec<String>,
    /// Commands to ask proj's jobs socket for.
    pub asks: Vec<Vec<String>>,
    /// Files to write in the workdir, as the plugin's user.
    pub plant: BTreeMap<PathBuf, String>,
    /// Seconds to wait before answering.
    pub sleep: u64,
}

/// What it could do.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Report {
    /// Each capability, by its grant's name: whether it worked.
    pub works: BTreeMap<String, bool>,
    /// What each said, for a check's failure to show.
    pub said: BTreeMap<String, String>,
    /// What it was told it holds.
    pub told: Vec<String>,
    /// The names in its environment.
    pub env: Vec<String>,
    pub proxied: BTreeMap<String, String>,
    pub dialed: Vec<String>,
    pub found: Vec<String>,
    pub readable: Vec<PathBuf>,
    pub written: Vec<PathBuf>,
    pub mounted: Vec<String>,
    pub asked: Vec<String>,
}

fn describe() -> Answer {
    let sandbox = if std::env::var_os(BRINGS).is_some() {
        Sandbox::Brings
    } else {
        Sandbox::Needs
    };
    Answer::Describe {
        versions: CONTRACTS.map(str::to_owned).to_vec(),
        kinds: vec![],
        v2_kinds: vec![Kind::Job],
        v4_kinds: vec![],
        models: vec![],
        capabilities: Some(Capabilities {
            sandbox,
            network: true,
            devices: vec!["kvm".to_owned()],
            mcp: true,
        }),
        meters: vec![],
    }
}

/// The plugin's answer to `request`.
pub fn answer(request: &Request) -> Result<Response, String> {
    let answer = match &request.task {
        Task::Describe => describe(),
        Task::Job {
            repo,
            workdir,
            prompt,
            ..
        } => {
            let conformance = fixtures().into_iter().any(|f| {
                matches!(f.task, Task::Job { repo: r, workdir: w, prompt: p, .. }
                    if r == *repo && w == *workdir && p == *prompt)
            });
            let summary = if conformance {
                "the fixture's job is finished".to_owned()
            } else {
                let look: Look = serde_json::from_str(prompt)
                    .map_err(|e| format!("the prompt isn't a look: {e}"))?;
                serde_json::to_string(&report(&look)).map_err(|e| e.to_string())?
            };
            Answer::Job {
                summary,
                next: Next::Done,
            }
        }
        _ => return Err("this plugin serves jobs alone".to_owned()),
    };
    Ok(Response {
        contract: request.contract.clone(),
        by: Identity {
            engine: "example".to_owned(),
            session: None,
        },
        execution: request.execution.as_ref().map(|asked| Executed {
            model: None,
            effort: None,
            context: asked.context.clone(),
        }),
        answer,
    })
}

fn report(look: &Look) -> Report {
    let mut report = Report {
        told: std::env::var("PROJ_CAPABILITIES")
            .ok()
            .and_then(|told| serde_json::from_str(&told).ok())
            .unwrap_or_default(),
        env: std::env::vars_os()
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect(),
        ..Report::default()
    };
    uses::every(look, &mut report);
    escape::every(look, &mut report);
    for (path, text) in &look.plant {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if std::fs::write(path, text).is_ok() {
            report.written.push(path.clone());
        }
    }
    if look.sleep > 0 {
        eprintln!("example plugin: waiting");
        std::thread::sleep(std::time::Duration::from_secs(look.sleep));
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conformance_gets_an_offline_answer() {
        let task = fixtures()
            .into_iter()
            .find(|f| matches!(f.task, Task::Job { .. }))
            .unwrap();
        let response = answer(&Request::work(task.contract, task.task.clone())).unwrap();
        assert!(task.conforms(&response.answer));
    }

    #[test]
    fn it_declares_the_whole_job_contract() {
        let Answer::Describe {
            v2_kinds,
            capabilities: Some(declared),
            ..
        } = answer(&Request::describe()).unwrap().answer
        else {
            panic!("not a describe");
        };
        assert_eq!(v2_kinds, [Kind::Job]);
        assert!(declared.network && declared.mcp);
        assert_eq!(declared.devices, ["kvm"]);
    }
}
