//! A test fixture: an engine plugin for the sandbox's checks
//! (tests/agents-plugins.nix, lib/engine-conformance.nix). It runs no model.
//!
//! It declares the whole job contract ([`DECLARES`] narrows it). Conary's
//! conformance job gets its offline answer. Any other job's prompt is a
//! [`Look`]: it tries every capability by the way a grant renders it,
//! whatever it was told it holds, and reports what worked ([`Report`]), so a
//! check sees what is withheld as well as what is granted. It uses the root
//! it has in its VM to try. Put around another plugin ([`AROUND`]), it tries
//! them under that plugin's describe, and [`holds`] says whether what worked
//! is what that plugin declares.

pub mod around;
mod escape;
mod uses;

use burrow_grants::Capability;

use conary_core::plugin::{
    Answer, CONTRACTS, Capabilities, Executed, Identity, Kind, Next, Request, Response, Sandbox,
    Task, fixtures,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The variable that has the fixture declare a sandbox of its own.
pub const BRINGS: &str = "EXAMPLE_PLUGIN_BRINGS_SANDBOX";

/// The variable that has it declare, of `network`, `mcp` and `kvm`, only
/// those it names (space-separated), rather than all three.
pub const DECLARES: &str = "EXAMPLE_PLUGIN_DECLARES";

/// The variable that puts it around the plugin at that path ([`around`]).
pub const AROUND: &str = "EXAMPLE_PLUGIN_AROUND";

/// Optional engine identity override (defaults to `"example"`).
pub const ENGINE: &str = "EXAMPLE_PLUGIN_ENGINE";

/// Optional JSON array of `conary_core::plugin::Model` declared by `describe`.
pub const MODELS: &str = "EXAMPLE_PLUGIN_MODELS";

/// Optional sandbox requirement override (`"none"`, `"needs"`, or `"brings"`).
pub const SANDBOX: &str = "EXAMPLE_PLUGIN_SANDBOX";

/// When non-empty, non-conformance `Task::Job` requests echo engine, model,
/// effort, and prompt as a completed job instead of parsing a `Look` JSON.
pub const ECHO_JOB: &str = "EXAMPLE_PLUGIN_ECHO_JOB";

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

fn engine_name() -> String {
    std::env::var(ENGINE)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "example".to_owned())
}

fn declared_models() -> Result<Vec<conary_core::plugin::Model>, String> {
    match std::env::var(MODELS).ok().filter(|s| !s.trim().is_empty()) {
        Some(raw) => serde_json::from_str(&raw).map_err(|e| format!("invalid {MODELS} JSON: {e}")),
        None => Ok(vec![]),
    }
}

fn describe() -> Result<Answer, String> {
    let sandbox = match std::env::var(SANDBOX).ok().as_deref() {
        Some("none") => Sandbox::None,
        Some("needs") => Sandbox::Needs,
        Some("brings") => Sandbox::Brings,
        Some(other) => return Err(format!("invalid {SANDBOX} value {other:?}")),
        None if std::env::var_os(BRINGS).is_some() => Sandbox::Brings,
        None => Sandbox::Needs,
    };
    let models = declared_models()?;
    let declares = std::env::var(DECLARES).ok();
    let declares = |field: &str| {
        declares
            .as_deref()
            .is_none_or(|names| names.split_whitespace().any(|n| n == field))
    };
    Ok(Answer::Describe {
        versions: CONTRACTS.map(str::to_owned).to_vec(),
        kinds: vec![],
        v2_kinds: vec![Kind::Job],
        v4_kinds: vec![],
        models,
        capabilities: Some(Capabilities {
            sandbox,
            network: declares("network"),
            devices: if declares("kvm") {
                vec!["kvm".to_owned()]
            } else {
                vec![]
            },
            mcp: declares("mcp"),
        }),
        meters: vec![],
    })
}

/// The plugin's answer to `request`.
pub fn answer(request: &Request) -> Result<Response, String> {
    let engine = engine_name();
    let default_model = declared_models()?.into_iter().next().map(|m| m.id);
    let mut executed_model = None;
    let mut executed_effort = None;
    let answer = match &request.task {
        Task::Describe => describe()?,
        Task::Job {
            prompt,
            model,
            effort,
            ..
        } => {
            executed_model = model.clone().or(default_model);
            executed_effort = effort.clone();
            let summary = if conformance(&request.task) {
                "the fixture's job is finished".to_owned()
            } else if std::env::var_os(ECHO_JOB).is_some_and(|v| !v.is_empty()) {
                format!(
                    "engine={engine} model={} effort={} prompt={prompt}",
                    model.as_deref().unwrap_or("-"),
                    effort.as_deref().unwrap_or("-"),
                )
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
            engine,
            session: None,
        },
        execution: request.execution.as_ref().map(|asked| Executed {
            model: executed_model.or_else(|| asked.choice.as_ref().and_then(|c| c.model.clone())),
            effort: executed_effort
                .or_else(|| asked.choice.as_ref().and_then(|c| c.effort.clone())),
            context: asked.context.clone(),
        }),
        answer,
    })
}

/// Whether `task` is one of Conary's conformance jobs, rather than a look.
pub fn conformance(task: &Task) -> bool {
    let Task::Job {
        repo,
        workdir,
        prompt,
        ..
    } = task
    else {
        return false;
    };
    fixtures().into_iter().any(|f| {
        matches!(f.task, Task::Job { repo: r, workdir: w, prompt: p, .. }
            if r == *repo && w == *workdir && p == *prompt)
    })
}

/// The environment variables that would carry a GitHub login, which no
/// plugin is granted.
const LOGINS: &[&str] = &["GH_TOKEN", "GITHUB_TOKEN", "GH_TOKEN_SOCKET"];

/// What's wrong, by `report`, with a plugin that declares `declared`: each
/// capability it declares, it requires, so that has to work under its
/// grant, and the plugin told of it; none it doesn't declare may work or be
/// told of, whatever the owner approved; and no GitHub login is there. Empty
/// when it holds.
pub fn holds(declared: &[Capability], report: &Report) -> Vec<String> {
    let mut wrong = Vec::new();
    let is_declared = |name: &str| declared.iter().any(|c| c.name() == name);
    let said = |name: &str| report.said.get(name).map_or("", String::as_str).trim();
    for name in declared.iter().map(|c| c.name()) {
        match report.works.get(name) {
            None => wrong.push(format!(
                "{name}: declared, and the fixture has no way to try it"
            )),
            Some(false) => wrong.push(format!(
                "{name}: declared, so required, and unavailable under its grant: {}",
                said(name)
            )),
            Some(true) => {}
        }
        if !report.told.iter().any(|t| t == name) {
            wrong.push(format!(
                "{name}: declared, and the plugin wasn't told it holds it"
            ));
        }
    }
    for (name, _) in report.works.iter().filter(|(_, works)| **works) {
        if !is_declared(name) {
            wrong.push(format!(
                "{name}: not declared, and available: {}",
                said(name)
            ));
        }
    }
    for name in report.told.iter().filter(|t| !is_declared(t)) {
        wrong.push(format!(
            "{name}: not declared, and the plugin was told it holds it"
        ));
    }
    for login in LOGINS.iter().filter(|l| report.env.iter().any(|e| e == *l)) {
        wrong.push(format!(
            "github: never granted, and {login} is in its environment"
        ));
    }
    wrong
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

    fn reported(works: &[(&str, bool)], told: &[&str]) -> Report {
        Report {
            works: works.iter().map(|&(n, w)| (n.to_owned(), w)).collect(),
            said: works
                .iter()
                .map(|&(n, _)| (n.to_owned(), format!("{n} said")))
                .collect(),
            told: told.iter().map(|&t| t.to_owned()).collect(),
            ..Report::default()
        }
    }

    #[test]
    fn it_holds_when_what_works_is_what_is_declared() {
        let declared = [Capability::Workspace, Capability::Network];
        let report = reported(
            &[("workspace", true), ("network", true), ("kvm", false)],
            &["workspace", "network"],
        );
        assert_eq!(holds(&declared, &report), Vec::<String>::new());
    }

    #[test]
    fn a_required_capability_that_does_not_work_fails_it_by_name() {
        let declared = [Capability::Workspace, Capability::Network];
        let report = reported(&[("workspace", true), ("network", false)], &["workspace"]);
        assert_eq!(
            holds(&declared, &report),
            [
                "network: declared, so required, and unavailable under its grant: network said",
                "network: declared, and the plugin wasn't told it holds it",
            ]
        );
    }

    #[test]
    fn an_undeclared_capability_that_works_fails_it() {
        let mut report = reported(&[("workspace", true), ("kvm", true)], &["workspace", "kvm"]);
        report.env = vec!["GH_TOKEN".to_owned()];
        assert_eq!(
            holds(&[Capability::Workspace], &report),
            [
                "kvm: not declared, and available: kvm said",
                "kvm: not declared, and the plugin was told it holds it",
                "github: never granted, and GH_TOKEN is in its environment",
            ]
        );
    }

    #[test]
    fn a_declared_capability_the_fixture_cannot_try_fails_it() {
        let report = reported(&[], &["environment"]);
        assert_eq!(
            holds(&[Capability::Environment], &report),
            ["environment: declared, and the fixture has no way to try it"]
        );
    }
}
