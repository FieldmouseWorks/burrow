//! `burrow-judge look CONFIG ID`, a look's worker: the process its unit
//! (`UNIT@ID.service`) starts, as the look's own user, which sees its
//! folder of the producer's state and nothing else of it: not its keys,
//! its records or its App. It runs the judging engine once, in a fresh
//! session, on what the producer laid out ([`crate::look::lay`]), and
//! leaves what it decided in the folder's answer ([`Looked`]); its exit is
//! 0 once that's written, anything else when it isn't.
//!
//! The engine is Claude Code, as the machine has it, with nothing of the
//! machine's or the PR's that could steer it: an empty home of its own
//! (the unit hides the machine's managed settings), no MCP servers,
//! `--restricted` (no settings files, no code-running tools, file tools
//! held to its folders), the read-only tools alone, and run from an empty
//! folder with the head and its base added, so no `CLAUDE.md` of the PR's
//! is read as instructions. Its login is the unit's systemd credential,
//! never the host's. Its one way out is the producer's gate
//! ([`crate::egress`]), as its HTTPS proxy.
use crate::egress;
use crate::look::{ANSWER, GATE, Looked, SPEC, Spec};
use conary_core::plugin::Judgment;
use serde::Deserialize;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The tools a look has: it reads, and changes nothing.
pub const TOOLS: &str = "Read,Grep,Glob";

/// What a look's worker needs.
pub struct Worker {
    /// Claude Code.
    pub claude: PathBuf,
    /// The look's folder.
    pub dir: PathBuf,
    /// An empty folder of the look's own: its home, and where it runs.
    pub home: PathBuf,
    /// Its login: a token from `claude setup-token`.
    pub token: Vec<u8>,
}

/// The output schema a look answers to.
fn schema() -> String {
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

/// Claude's arguments for `spec`, in `dir`.
pub fn argv(spec: &Spec, dir: &Path) -> Vec<String> {
    let mut args: Vec<String> = [
        "-p",
        "--output-format",
        "json",
        "--restricted",
        "--permission-mode",
        "dontAsk",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--tools",
        TOOLS,
        "--json-schema",
    ]
    .map(String::from)
    .to_vec();
    args.push(schema());
    args.push("--add-dir".into());
    args.push(dir.to_string_lossy().into_owned());
    args.extend(["--model".into(), spec.model.clone()]);
    if let Some(e) = &spec.effort {
        args.extend(["--effort".into(), e.clone()]);
    }
    args
}

/// What Claude printed, the parts a look reads.
#[derive(Deserialize)]
struct Printed {
    #[serde(default)]
    is_error: bool,
    subtype: Option<String>,
    session_id: Option<String>,
    structured_output: Option<Said>,
    #[serde(rename = "modelUsage", default)]
    model_usage: std::collections::BTreeMap<String, serde_json::Value>,
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

/// What a look decided, from what Claude printed for `spec`: its verdict
/// and reasons, by its session, on the model the look asked for, which
/// must be the one Claude says it ran.
pub fn looked(printed: &[u8], spec: &Spec) -> Result<Looked, String> {
    let p: Printed = serde_json::from_slice(printed)
        .map_err(|e| format!("Claude printed no result that reads: {e}"))?;
    if p.is_error || p.subtype.as_deref() != Some("success") {
        return Err(format!(
            "Claude's run failed ({})",
            p.subtype.as_deref().unwrap_or("no result")
        ));
    }
    let said = p.structured_output.ok_or("Claude gave no verdict")?;
    let session = p
        .session_id
        .filter(|s| !s.trim().is_empty())
        .ok_or("Claude's run names no session")?;
    if !p.model_usage.contains_key(&spec.model) {
        return Err(format!(
            "Claude ran {:?}, not {}",
            p.model_usage.keys().collect::<Vec<_>>(),
            spec.model
        ));
    }
    let looked = Looked {
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
        let token = String::from_utf8(self.token.clone())
            .map_err(|_| "its login isn't text".to_string())?;
        let proxy = egress::bridge(&self.dir.join(GATE)).map_err(|e| format!("its bridge: {e}"))?;
        let mut child = Command::new(&self.claude)
            .args(argv(&spec, &self.dir))
            .current_dir(&self.home)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &self.home)
            .env("TMPDIR", &self.home)
            .env("HTTPS_PROXY", &proxy)
            .env("https_proxy", &proxy)
            .env("CLAUDE_CODE_OAUTH_TOKEN", token.trim())
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{}: {e}", self.claude.display()))?;
        let mut stdin = child.stdin.take().ok_or("Claude's stdin")?;
        let prompt = spec.prompt.clone();
        let writer = std::thread::spawn(move || stdin.write_all(prompt.as_bytes()));
        let out = child
            .wait_with_output()
            .map_err(|e| format!("{}: {e}", self.claude.display()))?;
        let _ = writer.join();
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
