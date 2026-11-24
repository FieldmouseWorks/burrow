//! `burrow-judge look CONFIG ID`, a look's worker: the process its unit
//! (`UNIT@ID.service`) starts, as the look's own user, which sees its
//! folder of the producer's state and nothing else of it: not its keys,
//! its records or its App. It runs the selected modular Conary v5 engine
//! plugin once, in a fresh session, on what the producer laid out
//! ([`crate::look::lay`]), and leaves what it decided in the folder's answer
//! ([`Looked`]); its exit is 0 once that's written, anything else when it
//! isn't.
//!
//! No provider-specific CLI flags or environment variables live here: the
//! worker speaks `conary-plugin/v5` (`Task::Job` with `Context::Fresh`) to the
//! selected engine's configured plugin, passing an empty private `$HOME`, the
//! look folder via `BURROW_JUDGE_DIR`, the strict verdict JSON schema via
//! `BURROW_JUDGE_JSON_SCHEMA`, the engine's isolated login via its configured
//! `credential_env`, and the egress bridge via `HTTPS_PROXY`.

use crate::EngineConfig;
use crate::egress;
use crate::look::{ANSWER, GATE, Looked, SPEC, Spec};
use conary_core::plugin::{
    Answer, CONTRACT, Capabilities, Choice, Context, Execution, Identity, Judgment, Model,
    Negotiated, Offers, Request, Response, Sandbox, Task,
};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Environment variable naming the read-only look folder for the engine plugin.
pub const JUDGE_DIR_ENV: &str = "BURROW_JUDGE_DIR";

/// Environment variable carrying the strict JSON schema for the verdict output.
pub const JUDGE_SCHEMA_ENV: &str = "BURROW_JUDGE_JSON_SCHEMA";

/// Reads a named systemd credential (only the selected engine's credential is
/// loaded).
pub type CredentialReader = Box<dyn Fn(&str) -> Result<Vec<u8>, String>>;

/// What a look's worker needs.
pub struct Worker {
    /// Configured modular engine surfaces, keyed by engine name.
    pub engines: BTreeMap<String, EngineConfig>,
    /// The look's folder.
    pub dir: PathBuf,
    /// An empty folder of the look's own: its home, and where it runs.
    pub home: PathBuf,
    /// Loads a named systemd credential (only the selected engine's credential
    /// is read).
    pub read_credential: CredentialReader,
}

/// The strict JSON schema a look answers to.
pub fn schema() -> String {
    serde_json::json!({
        "type": "object",
        "properties": {
            "verdict": {"type": "string", "enum": ["pass", "fail", "owner", "misdescribed"]},
            "why": {"type": "string", "minLength": 1},
        },
        "required": ["verdict", "why"],
        "additionalProperties": false,
    })
    .to_string()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Said {
    verdict: Judgment,
    why: String,
}

/// How much of its login an answer may not hold: any run of this many of
/// its characters, more than the prefix every token shares.
const SHOWN: usize = 24;

/// Why `why` can't be posted for a look whose login is `token`: it holds
/// some of it. A head that talked the engine into quoting its login
/// answers nothing, rather than have it posted on the PR.
pub fn holds_login(why: &str, token: &str) -> Option<String> {
    let token: Vec<char> = token.trim().chars().collect();
    if token.is_empty() {
        return None;
    }
    let run = SHOWN.min(token.len());
    token
        .windows(run)
        .any(|w| why.contains(&w.iter().collect::<String>()))
        .then(|| "the judge's look quoted its own login: it answers nothing".to_string())
}

/// Build the `conary-plugin/v5` fresh-session job request sent to the selected
/// engine plugin.
pub fn plugin_request(spec: &Spec, workdir: &Path) -> Request {
    Request {
        contract: CONTRACT.into(),
        task: Task::Job {
            repo: spec.repo.clone(),
            workdir: workdir.to_string_lossy().into_owned(),
            prompt: spec.prompt.clone(),
            model: Some(spec.model.clone()),
            effort: spec.effort.clone(),
        },
        execution: Some(Execution {
            choice: Some(Choice {
                engine: spec.engine.clone(),
                model: Some(spec.model.clone()),
                effort: spec.effort.clone(),
            }),
            context: Context::Fresh,
        }),
        negotiated: Some(Negotiated {
            by: Identity {
                engine: spec.engine.clone(),
                session: None,
            },
            offers: Offers {
                models: vec![Model {
                    id: spec.model.clone(),
                    efforts: spec.effort.iter().cloned().collect(),
                    context_tokens: None,
                    max_output_tokens: None,
                }],
                capabilities: Capabilities {
                    sandbox: Sandbox::Brings,
                    network: true,
                    devices: vec![],
                    mcp: false,
                },
                meters: vec![],
            },
        }),
    }
}

/// Validate one `conary-plugin/v5` plugin stdout response against `spec`:
/// engine identity, fresh-session provenance, requested model/effort, and
/// strict structured `{verdict, why}` output.
pub fn looked(printed: &[u8], spec: &Spec) -> Result<Looked, String> {
    let response: Response = serde_json::from_slice(printed)
        .map_err(|e| format!("{} plugin printed no response that reads: {e}", spec.engine))?;
    if response.by.engine != spec.engine {
        return Err(format!(
            "engine plugin answered as {:?}, not {:?}",
            response.by.engine, spec.engine
        ));
    }
    let session = response
        .by
        .session
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("{} plugin run names no session", spec.engine))?;
    let executed = response
        .execution
        .ok_or_else(|| format!("{} plugin returned no execution provenance", spec.engine))?;
    if executed.context != Context::Fresh {
        return Err(format!(
            "{} plugin did not run a fresh session ({:?})",
            spec.engine, executed.context
        ));
    }
    if executed.model.as_deref() != Some(spec.model.as_str()) {
        return Err(format!(
            "{} plugin ran {:?}, not {}",
            spec.engine, executed.model, spec.model
        ));
    }
    if spec.effort.is_some() && executed.effort != spec.effort {
        return Err(format!(
            "{} plugin ran effort {:?}, not {:?}",
            spec.engine, executed.effort, spec.effort
        ));
    }
    let Answer::Job { summary, .. } = response.answer else {
        return Err(format!("{} plugin returned a non-job answer", spec.engine));
    };
    let said: Said = serde_json::from_str(summary.trim())
        .map_err(|e| format!("{} plugin gave no valid verdict JSON: {e}", spec.engine))?;
    let looked = Looked {
        engine: spec.engine.clone(),
        verdict: said.verdict,
        why: said.why,
        session,
        model: spec.model.clone(),
    };
    // Held to the producer's own reading of it, before it's left there.
    Looked::read(&serde_json::to_vec(&looked).map_err(|e| e.to_string())?)
}

impl Worker {
    /// The look, run: its answer left in its folder.
    pub fn look(&self) -> Result<(), String> {
        let at = |e: std::io::Error, path: &Path| format!("{}: {e}", path.display());
        let path = self.dir.join(SPEC);
        let spec: Spec = serde_json::from_slice(&fs::read(&path).map_err(|e| at(e, &path))?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let engine = self
            .engines
            .get(&spec.engine)
            .ok_or_else(|| format!("no judge engine surface is configured for {}", spec.engine))?;
        let token_bytes = (self.read_credential)(&engine.credential)?;
        let token = String::from_utf8(token_bytes)
            .map_err(|_| format!("{} login credential isn't UTF-8 text", spec.engine))?;
        let proxy = egress::bridge(&self.dir.join(GATE)).map_err(|e| format!("its bridge: {e}"))?;
        let request_bytes = serde_json::to_vec(&plugin_request(&spec, &self.home))
            .map_err(|e| format!("encode plugin request: {e}"))?;

        let mut child = Command::new(&engine.plugin)
            .current_dir(&self.home)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TMPDIR", &self.home)
            .env("HTTPS_PROXY", &proxy)
            .env("https_proxy", &proxy)
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("no_proxy", "127.0.0.1,localhost")
            .env(JUDGE_DIR_ENV, &self.dir)
            .env(JUDGE_SCHEMA_ENV, schema())
            .env(&engine.credential_env, token.trim())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| format!("{}: {e}", engine.plugin.display()))?;

        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("{} plugin stdin unavailable", spec.engine))?;
        let writer = std::thread::spawn(move || {
            let _ = stdin.write_all(&request_bytes);
            let _ = stdin.write_all(b"\n");
        });
        let out = child
            .wait_with_output()
            .map_err(|e| format!("{}: {e}", engine.plugin.display()))?;
        let _ = writer.join();
        if !out.status.success() {
            return Err(format!("{} plugin failed ({})", spec.engine, out.status));
        }
        let looked = looked(&out.stdout, &spec)
            .map_err(|why| format!("{why} (it exited {})", out.status))?;
        if let Some(why) = holds_login(&looked.why, &token) {
            return Err(why);
        }
        let path = self.dir.join(ANSWER);
        fs::write(
            &path,
            serde_json::to_vec(&looked).map_err(|e| e.to_string())?,
        )
        .map_err(|e| at(e, &path))
    }
}
