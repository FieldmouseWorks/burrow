//! Pinned engine plugin installation, verification, sandboxed `cn plugin check`,
//! owner account-ID authentication, and picker data for role/per-job choices
//! and dashboard presentation (#669).

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use burrow_github::Author;
use conary_core::plugin::{Kind, Limits, Plugin, Sandbox};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::roles::{Offer, plain, retired_engine, valid_engine_name};

mod store;
pub use store::{PickerData, PickerEngine};

/// Usage line for plugin commands across the host CLI and issue door.
pub const USAGE: &str = "usage: settings plugin list | plugin show ENGINE | plugin picker | plugin verify [ENGINE] | plugin install ENGINE nix|release SOURCE VERSION SHA256 | plugin update ENGINE nix|release SOURCE VERSION SHA256 | plugin remove ENGINE [--fallback FALLBACK]";

/// Maximum artifact size accepted for verification or download (256 MiB).
const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;

/// Maximum response bytes accepted during `cn plugin check` (1 MiB, matching Conary's default).
const CHECK_RESPONSE_BYTES: usize = 1024 * 1024;

/// Environment variables marking a proj job, plugin sandbox, autonomous agent
/// session, or GitHub App token socket. Any non-empty value refuses host CLI
/// plugin mutations so jobs and models acting alone can never install, update,
/// or remove engine plugins.
pub const JOB_OR_MODEL_ENV: &[&str] = &[
    "PROJ_JOB",
    "PROJ_HOST_JOBS_SOCKET",
    "PROJ_PROBE_SOCKET",
    "PROJ_VM_NEXT",
    "PROJ_CAPABILITIES",
    "CLAUDECODE",
    "ANTIGRAVITY_AGENT",
    "GH_TOKEN_SOCKET",
];

/// Verified owner identity authenticated by positive numeric GitHub account ID
/// (#660). Required for every plugin install, update, or removal mutation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerAuth {
    pub id: u64,
    pub login: String,
    pub actor: String,
}

impl OwnerAuth {
    /// Check that this identity matches one of `owners` by immutable numeric ID.
    /// Fails closed when no owners are configured.
    pub fn check(&self, owners: &[Author]) -> Result<(), String> {
        if owners.is_empty() {
            return Err(
                "no authorized owners are configured for engine plugin changes (burrow.settings.owners)"
                    .into(),
            );
        }
        if owners.iter().any(|owner| owner.matches(Some(self.id))) {
            Ok(())
        } else {
            Err(format!(
                "account id {} ({}) is not an authorized owner",
                self.id, self.login
            ))
        }
    }
}

/// Refuse execution if any job, sandbox, agent/model, or App-socket marker is
/// set in the environment.
pub fn reject_job_or_model_env() -> Result<(), String> {
    for var in JOB_OR_MODEL_ENV {
        if std::env::var_os(var).is_some_and(|v| !v.is_empty()) {
            return Err(format!(
                "plugin management is owner-only: refusing job or model caller ({var} is set)"
            ));
        }
    }
    Ok(())
}

/// Authenticate a host CLI caller against `owners` by numeric GitHub account ID
/// via `GET /user` on the caller's own `gh` (never the machinery App), after
/// rejecting any job or model environment.
pub fn authenticate_cli_owner(
    owners: &[Author],
    gh_bin: Option<&Path>,
) -> Result<OwnerAuth, String> {
    reject_job_or_model_env()?;
    if owners.is_empty() {
        return Err(
            "no authorized owners are configured for engine plugin changes (burrow.settings.owners)"
                .into(),
        );
    }
    let mut gh = burrow_github::Gh {
        program: gh_bin.map(|path| path.as_os_str().to_owned()),
        ..Default::default()
    };
    let response = burrow_github::call(&mut gh, "GET", "user", None, None)
        .map_err(|e| format!("couldn't authenticate owner identity via GitHub: {e}"))?;
    let body = response
        .json()
        .map_err(|e| format!("couldn't decode authenticated GitHub user: {e}"))?;
    let id = body
        .get("id")
        .and_then(serde_json::Value::as_u64)
        .filter(|id| *id > 0)
        .ok_or("GitHub user response lacked a positive numeric account id")?;
    let login = body
        .get("login")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_owned();
    let auth = OwnerAuth {
        id,
        login: login.clone(),
        actor: format!("host:id:{id}:{login}"),
    };
    auth.check(owners)?;
    Ok(auth)
}

/// How an installed engine plugin artifact was sourced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Nix,
    Release,
}

impl SourceKind {
    pub fn parse(word: &str) -> Option<Self> {
        match word {
            "nix" => Some(Self::Nix),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Nix => "nix",
            Self::Release => "release",
        }
    }
}

/// One model declared by a plugin's validated `describe` answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginModel {
    pub id: String,
    pub efforts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u64>,
}

/// Validated capabilities declared by a plugin's `describe` answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PluginCapabilities {
    pub sandbox: String,
    pub network: bool,
    pub mcp: bool,
    #[serde(default)]
    pub devices: Vec<String>,
    /// Grantable capability names from `burrow_grants::declared` (never
    /// includes forbidden `github` or `environment`).
    #[serde(default)]
    pub declared: Vec<String>,
}

/// One pinned engine plugin installed through the owner settings write door.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledPlugin {
    pub source_kind: SourceKind,
    pub source: String,
    pub path: PathBuf,
    pub version: String,
    pub sha256: String,
    pub models: Vec<PluginModel>,
    pub capabilities: PluginCapabilities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meters: Vec<String>,
    pub installed_at: i64,
    pub actor: String,
}

impl InstalledPlugin {
    /// Models and deduplicated effort union offered by this plugin's `describe`.
    pub fn offer(&self) -> Offer {
        let mut models = Vec::with_capacity(self.models.len());
        let mut efforts = Vec::new();
        let mut seen_efforts = BTreeSet::new();
        for model in &self.models {
            models.push(model.id.clone());
            for effort in &model.efforts {
                if seen_efforts.insert(effort.clone()) {
                    efforts.push(effort.clone());
                }
            }
        }
        Offer { models, efforts }
    }
}

/// One role route moved to the declared fallback during plugin removal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedRole {
    pub role: String,
    pub from: String,
    pub to: String,
}

/// Valid version string: non-empty ASCII alphanumerics plus `._+-`, max 128 chars.
pub fn valid_version(v: &str) -> bool {
    !v.is_empty()
        && v.len() <= 128
        && !v.starts_with('.')
        && !v.contains("..")
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

/// Normalize a SHA-256 hex pin (`sha256:<64-hex>` or `<64-hex>`) to lowercase
/// `sha256:<64-hex>`.
pub fn normalize_sha256(raw: &str) -> Result<String, String> {
    let hex = raw.strip_prefix("sha256:").unwrap_or(raw);
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "invalid sha256 pin {raw:?}: expected 64 hex characters (optionally prefixed with sha256:)"
        ));
    }
    Ok(format!("sha256:{}", hex.to_ascii_lowercase()))
}

/// Raw 64-char hex digest from a normalized `sha256:<hex>` pin.
pub(crate) fn hex_digest(normalized: &str) -> &str {
    normalized.strip_prefix("sha256:").unwrap_or(normalized)
}

/// Syntax validation for `plugin ...` commands (used by the issue door before
/// executing mutations).
pub fn valid_command(words: &[&str]) -> bool {
    match words {
        ["plugin", "list" | "picker" | "verify"] => true,
        ["plugin", "show" | "verify", engine] => valid_engine(engine).is_ok(),
        [
            "plugin",
            "install" | "update",
            engine,
            kind,
            source,
            version,
            sha256,
        ] => {
            valid_engine(engine).is_ok()
                && SourceKind::parse(kind).is_some()
                && valid_source_syntax(SourceKind::parse(kind).unwrap(), source)
                && valid_version(version)
                && normalize_sha256(sha256).is_ok()
        }
        ["plugin", "remove", engine] => valid_engine(engine).is_ok(),
        ["plugin", "remove", engine, "--fallback", fallback] => {
            valid_engine(engine).is_ok() && valid_engine(fallback).is_ok()
        }
        _ => false,
    }
}

pub(crate) fn valid_engine(name: &str) -> Result<(), String> {
    if valid_engine_name(name) && !retired_engine(name) {
        Ok(())
    } else {
        Err(format!("invalid engine plugin name {name:?}"))
    }
}

fn valid_source_syntax(kind: SourceKind, source: &str) -> bool {
    if source.is_empty() || source.contains(char::is_whitespace) || source.contains('\0') {
        return false;
    }
    match kind {
        SourceKind::Nix => source.starts_with('/'),
        SourceKind::Release => {
            source.starts_with('/')
                || source.starts_with("file:///")
                || source.starts_with("https://")
        }
    }
}

/// Compute `sha256:<64-lowercase-hex>` over a regular file's bytes, bounded by
/// [`MAX_ARTIFACT_BYTES`].
pub fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let meta = file
        .metadata()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: not a regular file", path.display()));
    }
    if meta.len() > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "{}: artifact exceeds {} byte limit",
            path.display(),
            MAX_ARTIFACT_BYTES
        ));
    }
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        if total > MAX_ARTIFACT_BYTES {
            return Err(format!(
                "{}: artifact exceeds {} byte limit",
                path.display(),
                MAX_ARTIFACT_BYTES
            ));
        }
        hasher.update(&buf[..n]);
    }
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("sha256:{hex}"))
}

/// Verify an already-installed plugin artifact on disk BEFORE executing it.
/// Called at every start (by `burrow-proj` before `describe` and before `spawn`,
/// and by `settings plugin verify`).
pub fn verify_installed(
    engine: &str,
    plugin: &InstalledPlugin,
    nix_store: &Path,
    plugins_dir: &Path,
) -> Result<PathBuf, String> {
    let expected = normalize_sha256(&plugin.sha256)?;
    match plugin.source_kind {
        SourceKind::Nix => {
            let store = fs::canonicalize(nix_store)
                .map_err(|e| format!("nix store {}: {e}", nix_store.display()))?;
            let canonical = fs::canonicalize(&plugin.path).map_err(|e| {
                format!(
                    "engine plugin {engine} artifact {}: {e}",
                    plugin.path.display()
                )
            })?;
            if !canonical.starts_with(&store) {
                return Err(format!(
                    "engine plugin {engine} Nix artifact {} is outside {}",
                    canonical.display(),
                    store.display()
                ));
            }
            let actual = sha256_file(&canonical)?;
            if actual != expected {
                return Err(format!(
                    "engine plugin {engine} artifact verification failed: expected {expected}, got {actual}"
                ));
            }
            Ok(canonical)
        }
        SourceKind::Release => {
            let meta = fs::symlink_metadata(&plugin.path).map_err(|e| {
                format!(
                    "engine plugin {engine} artifact {}: {e}",
                    plugin.path.display()
                )
            })?;
            if meta.file_type().is_symlink() || !meta.is_file() {
                return Err(format!(
                    "engine plugin {engine} release artifact {} must be a regular file, not a symlink",
                    plugin.path.display()
                ));
            }
            let root = fs::canonicalize(plugins_dir)
                .map_err(|e| format!("plugins dir {}: {e}", plugins_dir.display()))?;
            let canonical = fs::canonicalize(&plugin.path).map_err(|e| {
                format!(
                    "engine plugin {engine} artifact {}: {e}",
                    plugin.path.display()
                )
            })?;
            if !canonical.starts_with(&root) {
                return Err(format!(
                    "engine plugin {engine} release artifact {} is outside {}",
                    canonical.display(),
                    root.display()
                ));
            }
            let actual = sha256_file(&canonical)?;
            if actual != expected {
                return Err(format!(
                    "engine plugin {engine} artifact verification failed: expected {expected}, got {actual}"
                ));
            }
            Ok(canonical)
        }
    }
}

/// Temporary staging directory that removes itself on drop.
pub(crate) struct Staging {
    pub dir: PathBuf,
    pub binary: PathBuf,
    pub verified_source: String,
    pub sha256: String,
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Stage and hash-verify an artifact BEFORE setting executable bits or running
/// anything on it.
pub(crate) fn stage_and_verify(
    engine: &str,
    kind: SourceKind,
    source: &str,
    expected_sha256: &str,
    nix_store: &Path,
    plugins_dir: &Path,
    curl_bin: &Path,
) -> Result<Staging, String> {
    let expected = normalize_sha256(expected_sha256)?;
    let stage_root = plugins_dir.join(".staging");
    fs::create_dir_all(&stage_root).map_err(|e| format!("{}: {e}", stage_root.display()))?;
    let seq = STAGE_SEQ.fetch_add(1, Ordering::Relaxed);
    let dir = stage_root.join(format!("{engine}-{}-{seq}", std::process::id()));
    fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;

    let result = (|| -> Result<Staging, String> {
        match kind {
            SourceKind::Nix => {
                if !source.starts_with('/') {
                    return Err(format!(
                        "nix plugin source must be an absolute path inside {}",
                        nix_store.display()
                    ));
                }
                let store = fs::canonicalize(nix_store)
                    .map_err(|e| format!("nix store {}: {e}", nix_store.display()))?;
                let canonical = fs::canonicalize(source)
                    .map_err(|e| format!("nix plugin source {source}: {e}"))?;
                if !canonical.starts_with(&store) {
                    return Err(format!(
                        "nix plugin source {} is outside {}",
                        canonical.display(),
                        store.display()
                    ));
                }
                let actual = sha256_file(&canonical)?;
                if actual != expected {
                    return Err(format!(
                        "artifact hash mismatch for {engine} before execution: expected {expected}, got {actual}"
                    ));
                }
                Ok(Staging {
                    dir: dir.clone(),
                    binary: canonical.clone(),
                    verified_source: canonical.to_string_lossy().into_owned(),
                    sha256: expected,
                })
            }
            SourceKind::Release => {
                let staged_bin = dir.join("artifact");
                if source.starts_with("https://") {
                    fetch_https(curl_bin, source, &staged_bin)?;
                } else {
                    let local = source.strip_prefix("file://").unwrap_or(source);
                    if !local.starts_with('/') {
                        return Err(format!(
                            "release plugin source {source:?} must be an https:// URL, file:/// URL, or absolute path"
                        ));
                    }
                    copy_bounded(Path::new(local), &staged_bin)?;
                }
                // Verify SHA-256 BEFORE marking executable or running anything.
                let actual = sha256_file(&staged_bin)?;
                if actual != expected {
                    return Err(format!(
                        "artifact hash mismatch for {engine} before execution: expected {expected}, got {actual}"
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&staged_bin, fs::Permissions::from_mode(0o555))
                        .map_err(|e| format!("{}: {e}", staged_bin.display()))?;
                }
                Ok(Staging {
                    dir: dir.clone(),
                    binary: staged_bin,
                    verified_source: source.to_owned(),
                    sha256: expected,
                })
            }
        }
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    result
}

fn fetch_https(curl_bin: &Path, url: &str, dest: &Path) -> Result<(), String> {
    let out = Command::new(curl_bin)
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--proto",
            "=https",
            "--max-time",
            "60",
            "--max-filesize",
            &MAX_ARTIFACT_BYTES.to_string(),
            "--output",
        ])
        .arg(dest)
        .arg("--")
        .arg(url)
        .output()
        .map_err(|e| format!("failed to run {}: {e}", curl_bin.display()))?;
    if !out.status.success() {
        let detail = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(format!("failed to fetch release artifact {url}: {detail}"));
    }
    Ok(())
}

fn copy_bounded(src: &Path, dest: &Path) -> Result<(), String> {
    let mut input = File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let meta = input
        .metadata()
        .map_err(|e| format!("{}: {e}", src.display()))?;
    if !meta.is_file() {
        return Err(format!("{}: not a regular file", src.display()));
    }
    if meta.len() > MAX_ARTIFACT_BYTES {
        return Err(format!(
            "{}: artifact exceeds {} byte limit",
            src.display(),
            MAX_ARTIFACT_BYTES
        ));
    }
    let mut out = File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        out.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    let mut buf = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = input
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", src.display()))?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        if total > MAX_ARTIFACT_BYTES {
            return Err(format!(
                "{}: artifact exceeds {} byte limit",
                src.display(),
                MAX_ARTIFACT_BYTES
            ));
        }
        out.write_all(&buf[..n])
            .map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    out.sync_all()
        .map_err(|e| format!("{}: {e}", dest.display()))
}

/// Validated metadata returned by sandboxed `cn plugin check` before activation.
pub(crate) struct CheckedPlugin {
    pub models: Vec<PluginModel>,
    pub capabilities: PluginCapabilities,
    pub meters: Vec<String>,
}

/// Run Conary's `cn plugin check` (`Plugin::describe` + `Plugin::check`) inside
/// a confined bubblewrap sandbox (`--unshare-all --clearenv`, read-only plugin
/// mount, no network, no host home/state/secrets) before activating a plugin.
pub(crate) fn sandboxed_check(
    sandbox_bin: Option<&Path>,
    nix_store: &Path,
    stage_dir: &Path,
    engine: &str,
    binary: &Path,
    timeout_secs: u64,
) -> Result<CheckedPlugin, String> {
    let sandbox = sandbox_bin.ok_or(
        "sandboxed cn plugin check requires a configured sandbox runner (burrow.settings.plugins.sandbox)",
    )?;
    let wrapper = stage_dir.join("sandbox-exec");
    write_bwrap_wrapper(&wrapper, sandbox, nix_store, binary)?;

    let limits = Limits {
        deadline: Duration::from_secs(timeout_secs.max(1)),
        response_bytes: CHECK_RESPONSE_BYTES,
    };
    let plugin = Plugin::describe(&wrapper, limits)
        .map_err(|e| format!("sandboxed cn plugin check describe failed for {engine}: {e}"))?;

    if plugin.identity.engine != engine {
        return Err(format!(
            "sandboxed cn plugin check rejected {engine}: plugin reported engine identity {:?}",
            plugin.identity.engine
        ));
    }
    if !plugin.kinds().contains(&Kind::Job) {
        return Err(format!(
            "sandboxed cn plugin check rejected {engine}: plugin does not serve the job kind"
        ));
    }
    let offers = plugin
        .offers()
        .ok_or_else(|| format!("{engine}: plugin must speak conary-plugin v2+ with offers"))?
        .clone();

    for model in &offers.models {
        if !plain(&model.id) {
            return Err(format!(
                "{engine}: declared model id {:?} is not a plain identifier",
                model.id
            ));
        }
        for effort in &model.efforts {
            if !plain(effort) {
                return Err(format!(
                    "{engine}: declared effort {:?} for model {:?} is not a plain identifier",
                    effort, model.id
                ));
            }
        }
    }

    let checked_kinds = plugin
        .check()
        .map_err(|e| format!("sandboxed cn plugin check failed for {engine}: {e}"))?;
    if !checked_kinds.contains(&Kind::Job) {
        return Err(format!(
            "sandboxed cn plugin check for {engine} did not verify the job kind"
        ));
    }

    let sandbox_name = match offers.capabilities.sandbox {
        Sandbox::None => "none",
        Sandbox::Brings => "brings",
        Sandbox::Needs => "needs",
    };
    let declared = burrow_grants::declared(plugin.kinds(), &offers.capabilities)
        .into_iter()
        .map(|cap| cap.name().to_owned())
        .collect();

    Ok(CheckedPlugin {
        models: offers
            .models
            .into_iter()
            .map(|m| PluginModel {
                id: m.id,
                efforts: m.efforts,
                context_tokens: m.context_tokens,
                max_output_tokens: m.max_output_tokens,
            })
            .collect(),
        capabilities: PluginCapabilities {
            sandbox: sandbox_name.to_owned(),
            network: offers.capabilities.network,
            mcp: offers.capabilities.mcp,
            devices: offers.capabilities.devices,
            declared,
        },
        meters: offers.meters,
    })
}

/// Build the confinement wrapper script that executes `binary` inside `bwrap`
/// with `--unshare-all --die-with-parent --new-session --clearenv`.
fn write_bwrap_wrapper(
    wrapper: &Path,
    sandbox: &Path,
    nix_store: &Path,
    binary: &Path,
) -> Result<(), String> {
    let mut args: Vec<String> = vec![
        sandbox.to_string_lossy().into_owned(),
        "--unshare-all".into(),
        "--die-with-parent".into(),
        "--new-session".into(),
        "--clearenv".into(),
        "--setenv".into(),
        "PATH".into(),
        "/bin:/usr/bin".into(),
    ];
    if nix_store.exists() {
        let store = nix_store.to_string_lossy().into_owned();
        args.extend(["--ro-bind".into(), store.clone(), store]);
    }
    for dir in ["/bin", "/usr", "/lib", "/lib64"] {
        args.extend(["--ro-bind-try".into(), dir.into(), dir.into()]);
    }
    args.extend([
        "--ro-bind".into(),
        binary.to_string_lossy().into_owned(),
        "/plugin/bin".into(),
        "--tmpfs".into(),
        "/tmp".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--chdir".into(),
        "/tmp".into(),
        "--".into(),
        "/plugin/bin".into(),
    ]);
    let quoted: Vec<String> = args.iter().map(|a| sh_quote(a)).collect();
    let script = format!("#!/bin/sh\nexec {}\n", quoted.join(" "));
    let mut file = File::create(wrapper).map_err(|e| format!("{}: {e}", wrapper.display()))?;
    file.write_all(script.as_bytes())
        .map_err(|e| format!("{}: {e}", wrapper.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("{}: {e}", wrapper.display()))?;
    }
    file.sync_all()
        .map_err(|e| format!("{}: {e}", wrapper.display()))?;
    drop(file);
    Ok(())
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests;
