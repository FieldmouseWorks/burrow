//! Atomic store operations for pinned engine plugins inside `overlay.roles.json`:
//! install, update, removal with atomic role fallback, verification, picker
//! data for dashboard presentation (#669), and live JSON Schema enrichment.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::{
    InstalledPlugin, MovedRole, OwnerAuth, PluginCapabilities, PluginModel, SourceKind,
    format_install_answer, format_removal_answer, hex_digest, normalize_sha256, sandboxed_check,
    stage_and_verify, valid_engine, valid_version, verify_installed,
};
use crate::roles::{Hop, Roles, Route, default_routes};
use crate::{Config, live, now};

/// Picker metadata for one available engine (Nix-configured or installed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerEngine {
    pub engine: String,
    pub installed: bool,
    pub removable: bool,
    pub models: Vec<PluginModel>,
    pub efforts: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capabilities: Option<PluginCapabilities>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub meters: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_kind: Option<SourceKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

/// Complete picker payload for role routing, per-job overrides, and dashboard
/// presentation (#669).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PickerData {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_fallback: Option<String>,
    pub engines: BTreeMap<String, PickerEngine>,
}

impl Config {
    /// Directory storing verified release plugin artifacts and staging files.
    pub fn plugins_dir(&self) -> PathBuf {
        self.plugins_dir.clone().unwrap_or_else(|| {
            self.overlay
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("plugins")
        })
    }

    /// Configured Nix store root (defaults to `/nix/store`).
    pub fn nix_store(&self) -> PathBuf {
        self.nix_store
            .clone()
            .unwrap_or_else(|| PathBuf::from("/nix/store"))
    }

    /// Configured curl binary (defaults to `curl` on `PATH`).
    fn curl_bin(&self) -> PathBuf {
        self.plugin_curl
            .clone()
            .unwrap_or_else(|| PathBuf::from("curl"))
    }

    /// Per-invocation `cn plugin check` timeout in seconds.
    fn check_timeout_secs(&self) -> u64 {
        self.plugin_check_timeout_seconds.unwrap_or(30)
    }

    /// All currently installed pinned engine plugins.
    pub fn installed_plugins(&self) -> Result<BTreeMap<String, InstalledPlugin>, String> {
        Ok(self.role_view()?.plugins)
    }

    /// One installed pinned engine plugin by name.
    pub fn installed_plugin(&self, engine: &str) -> Result<Option<InstalledPlugin>, String> {
        Ok(self.installed_plugins()?.remove(engine))
    }

    /// Verify an installed plugin artifact on disk before executing it.
    pub fn verify_installed_plugin(&self, engine: &str) -> Result<PathBuf, String> {
        let plugin = self
            .installed_plugin(engine)?
            .ok_or_else(|| format!("engine plugin {engine} is not installed"))?;
        verify_installed(engine, &plugin, &self.nix_store(), &self.plugins_dir())
    }

    /// Structured picker data combining Nix-configured engines and installed
    /// plugins with their validated models, efforts, and capabilities.
    pub fn picker(&self) -> Result<PickerData, String> {
        let view = self.role_view()?;
        let mut engines = BTreeMap::new();
        for (name, offer) in &view.in_force.offers {
            if let Some(installed) = view.plugins.get(name) {
                let models = view
                    .in_force
                    .plugin_models
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| {
                        offer
                            .models
                            .iter()
                            .map(|id| PluginModel {
                                id: id.clone(),
                                efforts: offer.efforts.clone(),
                                context_tokens: None,
                                max_output_tokens: None,
                            })
                            .collect()
                    });
                engines.insert(
                    name.clone(),
                    PickerEngine {
                        engine: name.clone(),
                        installed: true,
                        removable: true,
                        models,
                        efforts: offer.efforts.clone(),
                        capabilities: Some(installed.capabilities.clone()),
                        meters: installed.meters.clone(),
                        source_kind: Some(installed.source_kind),
                        source: Some(installed.source.clone()),
                        version: Some(installed.version.clone()),
                        sha256: Some(installed.sha256.clone()),
                    },
                );
            } else {
                engines.insert(
                    name.clone(),
                    PickerEngine {
                        engine: name.clone(),
                        installed: false,
                        removable: false,
                        models: offer
                            .models
                            .iter()
                            .map(|id| PluginModel {
                                id: id.clone(),
                                efforts: offer.efforts.clone(),
                                context_tokens: None,
                                max_output_tokens: None,
                            })
                            .collect(),
                        efforts: offer.efforts.clone(),
                        capabilities: None,
                        meters: Vec::new(),
                        source_kind: None,
                        source: None,
                        version: None,
                        sha256: None,
                    },
                );
            }
        }
        Ok(PickerData {
            default_fallback: self.plugin_fallback.clone(),
            engines,
        })
    }

    /// JSON Schema enriched with live role engine choices, per-engine model /
    /// effort / capability offers, and `$defs.jobChoice` for per-job pickers.
    pub fn live_schema(&self) -> Result<Value, String> {
        let picker = self.picker()?;
        Ok(enrich_schema(&picker))
    }

    /// Read-only `plugin list | show | picker | verify` commands.
    pub(crate) fn plugin_read_command(&self, words: &[&str]) -> Result<String, String> {
        match words {
            ["plugin", "list"] => {
                let plugins = self.installed_plugins()?;
                if plugins.is_empty() {
                    return Ok("No installed engine plugins.\n".into());
                }
                let mut out = String::new();
                for (name, p) in plugins {
                    let short: String = hex_digest(&p.sha256).chars().take(12).collect();
                    let models: Vec<&str> = p.models.iter().map(|m| m.id.as_str()).collect();
                    out.push_str(&format!(
                        "{name} {} {} sha256:{short} models=[{}] sandbox={}\n",
                        p.source_kind.name(),
                        p.version,
                        models.join(","),
                        p.capabilities.sandbox,
                    ));
                }
                Ok(out)
            }
            ["plugin", "show", engine] => {
                valid_engine(engine)?;
                let plugin = self
                    .installed_plugin(engine)?
                    .ok_or_else(|| format!("engine plugin {engine} is not installed"))?;
                serde_json::to_string_pretty(&plugin)
                    .map(|s| s + "\n")
                    .map_err(|e| e.to_string())
            }
            ["plugin", "picker"] => serde_json::to_string_pretty(&self.picker()?)
                .map(|s| s + "\n")
                .map_err(|e| e.to_string()),
            ["plugin", "verify"] => {
                let plugins = self.installed_plugins()?;
                if plugins.is_empty() {
                    return Ok("No installed engine plugins to verify.\n".into());
                }
                let mut out = String::new();
                for (name, plugin) in &plugins {
                    let path =
                        verify_installed(name, plugin, &self.nix_store(), &self.plugins_dir())?;
                    out.push_str(&format!(
                        "{name} verified {} ({})\n",
                        plugin.sha256,
                        path.display()
                    ));
                }
                Ok(out)
            }
            ["plugin", "verify", engine] => {
                valid_engine(engine)?;
                let plugin = self
                    .installed_plugin(engine)?
                    .ok_or_else(|| format!("engine plugin {engine} is not installed"))?;
                let path =
                    verify_installed(engine, &plugin, &self.nix_store(), &self.plugins_dir())?;
                Ok(format!(
                    "{engine} verified {} ({})\n",
                    plugin.sha256,
                    path.display()
                ))
            }
            _ => Err(super::USAGE.into()),
        }
    }

    /// Execute a settings command on behalf of an authenticated owner identity.
    pub fn command_as(&self, auth: &OwnerAuth, args: &[String]) -> Result<String, String> {
        self.change_as(auth, args, None)
    }

    /// Execute a receipt-bound settings request on behalf of an authenticated
    /// owner identity (used by `burrow-settings-door`).
    pub fn request_as(
        &self,
        auth: &OwnerAuth,
        receipt: &Path,
        args: &[String],
    ) -> Result<String, String> {
        self.change_as(auth, args, Some(receipt))
    }

    fn change_as(
        &self,
        auth: &OwnerAuth,
        args: &[String],
        receipt: Option<&Path>,
    ) -> Result<String, String> {
        let words: Vec<&str> = args.iter().map(String::as_str).collect();
        match words.as_slice() {
            ["plugin", "list" | "show" | "picker" | "verify", ..] => {
                return self.plugin_read_command(&words);
            }
            ["plugin", ..] => {}
            _ => {
                return match receipt {
                    Some(path) => self.request(path, args, &auth.actor),
                    None => self.command(args, &auth.actor),
                };
            }
        }
        auth.check(&self.owners)?;
        let key = receipt.map(crate::receipt_key).transpose()?;
        match words.as_slice() {
            [
                "plugin",
                verb @ ("install" | "update"),
                engine,
                kind,
                source,
                version,
                sha256,
            ] => {
                let kind = SourceKind::parse(kind).ok_or_else(|| {
                    format!("unknown plugin source kind {kind:?} (use nix or release)")
                })?;
                self.install_or_update_plugin(
                    auth,
                    *verb == "update",
                    engine,
                    kind,
                    source,
                    version,
                    sha256,
                    args,
                    key.as_deref(),
                )
            }
            ["plugin", "remove", engine] => {
                self.remove_plugin(auth, engine, None, args, key.as_deref())
            }
            ["plugin", "remove", engine, "--fallback", fallback] => {
                self.remove_plugin(auth, engine, Some(*fallback), args, key.as_deref())
            }
            _ => Err(super::USAGE.into()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn install_or_update_plugin(
        &self,
        auth: &OwnerAuth,
        is_update: bool,
        engine: &str,
        kind: SourceKind,
        source: &str,
        version: &str,
        sha256: &str,
        args: &[String],
        receipt: Option<&str>,
    ) -> Result<String, String> {
        valid_engine(engine)?;
        if !valid_version(version) {
            return Err(format!("invalid plugin version {version:?}"));
        }
        let normalized_sha = normalize_sha256(sha256)?;
        if !super::valid_source_syntax(kind, source) {
            return Err(format!("invalid {} plugin source {source:?}", kind.name()));
        }

        // Fast replay check before staging/executing anything.
        if let Some(r) = receipt
            && let Some(prev) = self.roles_answered_key(r)?
        {
            return Ok(prev);
        }
        self.precheck_plugin_target(is_update, engine)?;

        // 1. Stage and verify SHA-256 BEFORE executing anything.
        let plugins_dir = self.plugins_dir();
        let nix_store = self.nix_store();
        let staging = stage_and_verify(
            engine,
            kind,
            source,
            &normalized_sha,
            &nix_store,
            &plugins_dir,
            &self.curl_bin(),
        )?;

        // 2. Run sandboxed `cn plugin check` before activation.
        let checked = sandboxed_check(
            self.plugin_sandbox.as_deref(),
            &nix_store,
            &staging.dir,
            engine,
            &staging.binary,
            self.check_timeout_secs(),
        )?;

        // 3. For release artifacts, materialize into content-addressed slot
        //    beside any prior version so the prior active installation stays
        //    untouched if the commit fails.
        let final_path = match kind {
            SourceKind::Nix => staging.binary.clone(),
            SourceKind::Release => {
                publish_release_artifact(&plugins_dir, engine, &staging.binary, &staging.sha256)?
            }
        };
        let created_release = (kind == SourceKind::Release).then(|| final_path.clone());

        let record = InstalledPlugin {
            source_kind: kind,
            source: staging.verified_source.clone(),
            path: final_path.clone(),
            version: version.to_owned(),
            sha256: staging.sha256.clone(),
            models: checked.models,
            capabilities: checked.capabilities,
            meters: checked.meters,
            installed_at: now(),
            actor: auth.actor.clone(),
        };
        // Re-verify the materialized file at its final path before publishing.
        verify_installed(engine, &record, &nix_store, &plugins_dir)?;

        let mut old_release_to_clean: Option<PathBuf> = None;
        let result = live::write_audited(
            &self.roles_file(),
            &self.overlay.with_extension("lock"),
            &self.overlay.with_extension("log"),
            &auth.actor,
            receipt,
            args,
            |doc: &mut crate::roles::store::Document| {
                self.commit_installed_plugin(
                    doc,
                    auth,
                    is_update,
                    engine,
                    &record,
                    &mut old_release_to_clean,
                )
            },
        );

        match result {
            Ok(answer) => {
                if let Some(old_path) = old_release_to_clean {
                    let _ = fs::remove_file(old_path);
                }
                Ok(answer.unwrap_or_default())
            }
            Err(err) => {
                // Clean up newly materialized release file if commit failed
                // (never deleting an existing file if the hash matched).
                if let Some(created) = created_release
                    && !self
                        .installed_plugin(engine)
                        .ok()
                        .flatten()
                        .is_some_and(|p| p.path == created)
                {
                    let _ = fs::remove_file(created);
                }
                Err(err)
            }
        }
    }

    fn commit_installed_plugin(
        &self,
        doc: &mut crate::roles::store::Document,
        auth: &OwnerAuth,
        is_update: bool,
        engine: &str,
        record: &InstalledPlugin,
        old_release_to_clean: &mut Option<PathBuf>,
    ) -> Result<live::Outcome, String> {
        self.ready_doc(doc, true)?;
        let previous = doc.plugins.get(engine).cloned();
        if is_update && previous.is_none() {
            return Err(format!(
                "engine plugin {engine} is not installed; use plugin install first"
            ));
        }
        if !is_update && previous.is_some() {
            return Err(format!(
                "engine plugin {engine} is already installed; use plugin update to replace it"
            ));
        }

        let mut next_plugins = doc.plugins.clone();
        next_plugins.insert(engine.to_owned(), record.clone());
        let mut next_offers = doc.offers.clone();
        // The plugin's validated describe offers take immediate effect.
        next_offers.remove(engine);
        // Reinstalling an engine clears any previous removal fallback.
        let mut next_removed_fallbacks = doc.removed_fallbacks.clone();
        next_removed_fallbacks.remove(engine);

        // Validate existing roles and tunings against the new offers so
        // an incompatible update fails cleanly and preserves the prior
        // active installation.
        doc.routing.in_force_with_plugins(
            &next_offers,
            &self.engines,
            &next_plugins,
            &next_removed_fallbacks,
        )?;

        if let Some(prev) = &previous
            && prev.source_kind == SourceKind::Release
            && prev.path != record.path
        {
            *old_release_to_clean = Some(prev.path.clone());
        }

        doc.plugins = next_plugins;
        doc.offers = next_offers;
        doc.removed_fallbacks = next_removed_fallbacks;
        doc.migrated.get_or_insert_with(now);

        let mut audit = Map::new();
        audit.insert("actorId".into(), json!(auth.id));
        audit.insert(
            "plugin".into(),
            json!({
                "operation": if is_update { "update" } else { "install" },
                "engine": engine,
                "sourceKind": record.source_kind,
                "source": record.source,
                "version": record.version,
                "sha256": record.sha256,
                "previousSource": previous.as_ref().map(|p| &p.source),
                "previousVersion": previous.as_ref().map(|p| &p.version),
                "previousSha256": previous.as_ref().map(|p| &p.sha256),
            }),
        );

        Ok(live::Outcome {
            answer: Some(format_install_answer(is_update, engine, record)),
            audit: Some(audit),
        })
    }

    fn remove_plugin(
        &self,
        auth: &OwnerAuth,
        engine: &str,
        explicit_fallback: Option<&str>,
        args: &[String],
        receipt: Option<&str>,
    ) -> Result<String, String> {
        valid_engine(engine)?;
        let fallback_opt = explicit_fallback.or(self.plugin_fallback.as_deref());
        let fallback = fallback_opt
            .ok_or_else(|| {
                format!(
                    "cannot remove engine plugin {engine} without a valid fallback engine (configure burrow.settings.plugins.fallback or pass --fallback ENGINE)"
                )
            })?
            .to_owned();
        valid_engine(&fallback)?;
        if fallback == engine {
            return Err(format!(
                "cannot remove engine plugin {engine}: fallback engine cannot be {engine} itself"
            ));
        }

        let mut removed_release_path: Option<PathBuf> = None;
        let result = live::write_audited(
            &self.roles_file(),
            &self.overlay.with_extension("lock"),
            &self.overlay.with_extension("log"),
            &auth.actor,
            receipt,
            args,
            |doc: &mut crate::roles::store::Document| {
                self.commit_plugin_removal(doc, auth, engine, &fallback, &mut removed_release_path)
            },
        )?;

        if let Some(path) = removed_release_path {
            let _ = fs::remove_file(path);
        }
        Ok(result.unwrap_or_default())
    }

    fn commit_plugin_removal(
        &self,
        doc: &mut crate::roles::store::Document,
        auth: &OwnerAuth,
        engine: &str,
        fallback: &str,
        removed_release_path: &mut Option<PathBuf>,
    ) -> Result<live::Outcome, String> {
        self.ready_doc(doc, true)?;
        let removed = doc
            .plugins
            .get(engine)
            .cloned()
            .ok_or_else(|| format!("engine plugin {engine} is not installed"))?;

        let mut next_plugins = doc.plugins.clone();
        next_plugins.remove(engine);
        let mut next_offers = doc.offers.clone();
        next_offers.remove(engine);

        // Verify fallback is a valid remaining active engine.
        let remaining_offers =
            crate::roles::merge_offers_with_plugins(&next_offers, &self.engines, &next_plugins)?;
        if !remaining_offers.contains_key(fallback) {
            return Err(format!(
                "cannot remove engine plugin {engine}: fallback engine {fallback:?} is not a valid remaining engine"
            ));
        }

        // Atomically rewrite affected live roles and clean removed
        // engine tunings.
        let mut next_routing = doc.routing.clone();
        let moved_roles = rewrite_roles_for_removal(&mut next_routing, engine, fallback)?;

        // Also rewrite any saved/previous setups referencing the
        // removed engine so they stay valid.
        let mut next_setups = doc.setups.clone();
        let mut moved_setups = Vec::new();
        for (setup_name, setup) in &mut next_setups.saved {
            let moved = rewrite_roles_for_removal(setup, engine, fallback)?;
            if !moved.is_empty() {
                moved_setups.push(setup_name.clone());
            }
        }
        if let Some(prev) = &mut next_setups.previous {
            rewrite_roles_for_removal(prev, engine, fallback)?;
        }

        // Record the fallback for persisted job pins (from `--engine`
        // or inbox labels), updating any transitive chains that pointed
        // to the engine being removed.
        let mut next_removed_fallbacks = doc.removed_fallbacks.clone();
        for target in next_removed_fallbacks.values_mut() {
            if target == engine {
                *target = fallback.to_owned();
            }
        }
        next_removed_fallbacks.insert(engine.to_owned(), fallback.to_owned());

        // Validate live post-removal routing completely before
        // committing. Saved/previous setups may already hold unrelated
        // stale tunings from earlier `offer` or `plugin update` changes
        // (checked at activation time by `engines use`), so verify here
        // only that no reference to the removed engine remains and that
        // every route stays structurally valid.
        next_routing.in_force_with_plugins(
            &next_offers,
            &self.engines,
            &next_plugins,
            &next_removed_fallbacks,
        )?;
        for (setup_name, setup) in &next_setups.saved {
            ensure_removed_engine_absent(setup, engine)
                .map_err(|e| format!("setup {setup_name}: {e}"))?;
        }
        if let Some(prev) = &next_setups.previous {
            ensure_removed_engine_absent(prev, engine)
                .map_err(|e| format!("previous setup: {e}"))?;
        }

        if removed.source_kind == SourceKind::Release {
            *removed_release_path = Some(removed.path.clone());
        }

        doc.plugins = next_plugins;
        doc.offers = next_offers;
        doc.routing = next_routing;
        doc.setups = next_setups;
        doc.removed_fallbacks = next_removed_fallbacks;
        doc.migrated.get_or_insert_with(now);

        let mut audit = Map::new();
        audit.insert("actorId".into(), json!(auth.id));
        audit.insert(
            "plugin".into(),
            json!({
                "operation": "remove",
                "engine": engine,
                "sourceKind": removed.source_kind,
                "source": removed.source,
                "version": removed.version,
                "sha256": removed.sha256,
                "fallback": fallback,
                "movedRoles": moved_roles,
                "movedSetups": moved_setups,
            }),
        );

        Ok(live::Outcome {
            answer: Some(format_removal_answer(
                engine,
                fallback,
                &removed,
                &moved_roles,
                &moved_setups,
            )),
            audit: Some(audit),
        })
    }

    fn precheck_plugin_target(&self, is_update: bool, engine: &str) -> Result<(), String> {
        // Check static collision before executing anything.
        let static_engines = self.static_engines();
        if static_engines.iter().any(|e| e == engine) {
            return Err(format!(
                "engine {engine} is configured statically in the machine flake and cannot be replaced via live plugin install"
            ));
        }
        // Pre-check install vs update existence before staging/running checks.
        let current = self.installed_plugins()?;
        if is_update && !current.contains_key(engine) {
            return Err(format!(
                "engine plugin {engine} is not installed; use plugin install first"
            ));
        }
        if !is_update && current.contains_key(engine) {
            return Err(format!(
                "engine plugin {engine} is already installed; use plugin update to replace it"
            ));
        }
        Ok(())
    }

    fn static_engines(&self) -> Vec<String> {
        if self.engines.is_empty() {
            default_routes()
                .values()
                .map(|r| r.engine.clone())
                .chain(std::iter::once(crate::credit::ENGINE.to_owned()))
                .collect()
        } else {
            self.engines.clone()
        }
    }
}

/// Publish a verified release artifact into `<plugins_dir>/<engine>/<hex>` via
/// atomic rename with mode `0o555`.
fn publish_release_artifact(
    plugins_dir: &Path,
    engine: &str,
    staged_bin: &Path,
    sha256: &str,
) -> Result<PathBuf, String> {
    let dir = plugins_dir.join(engine);
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dest = dir.join(hex_digest(sha256));
    let tmp = dir.join(format!(
        ".pending-{}-{}",
        std::process::id(),
        hex_digest(sha256)
    ));
    let bytes = fs::read(staged_bin).map_err(|e| format!("{}: {e}", staged_bin.display()))?;
    let mut file = File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    file.write_all(&bytes)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o555))
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
    }
    file.sync_all()
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(file);
    fs::rename(&tmp, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    File::open(&dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dest)
}

/// Rewrite every route in `roles` mentioning `removed` to use `fallback`,
/// deduplicating engines along each route, and delete any model tuning entries
/// targeting `removed`.
fn rewrite_roles_for_removal(
    roles: &mut Roles,
    removed: &str,
    fallback: &str,
) -> Result<Vec<MovedRole>, String> {
    let mut moved = Vec::new();
    for (role_key, route) in &mut roles.roles {
        if !route.engines().any(|e| e == removed) {
            continue;
        }
        let from = route.text();
        let next = rewrite_route(route, removed, fallback);
        let to = next.text();
        *route = next;
        moved.push(MovedRole {
            role: role_key.clone(),
            from,
            to,
        });
    }
    roles
        .models
        .retain(|key, _| key.split_once(' ').is_none_or(|(_, on)| on != removed));
    Ok(moved)
}

/// Verify no route or model tuning in `roles` still references `removed` and
/// every route remains structurally valid after rewriting.
fn ensure_removed_engine_absent(roles: &Roles, removed: &str) -> Result<(), String> {
    for (key, route) in &roles.roles {
        if route.engines().any(|e| e == removed) {
            return Err(format!(
                "role {key}: still references removed engine {removed}"
            ));
        }
        route.check().map_err(|e| format!("role {key}: {e}"))?;
    }
    for key in roles.models.keys() {
        if key.split_once(' ').is_some_and(|(_, on)| on == removed) {
            return Err(format!(
                "model {key}: still references removed engine {removed}"
            ));
        }
    }
    Ok(())
}

/// Replace `removed` with `fallback` in `route`, keeping each engine at most
/// once in order.
fn rewrite_route(route: &Route, removed: &str, fallback: &str) -> Route {
    let first = if route.engine == removed {
        fallback.to_owned()
    } else {
        route.engine.clone()
    };
    let mut seen = vec![first.clone()];
    let mut over = Vec::new();
    for hop in &route.over {
        let target = if hop.engine == removed {
            fallback.to_owned()
        } else {
            hop.engine.clone()
        };
        if !seen.contains(&target) {
            seen.push(target.clone());
            over.push(Hop {
                pct: hop.pct,
                engine: target,
            });
        }
    }
    Route {
        engine: first,
        over,
    }
}

/// Enrich [`crate::schema::json_schema`] with live available engine, model,
/// effort, and capability choices for role routing, per-job overrides
/// (`$defs.jobChoice`), and dashboard presentation (`x-picker`, #669).
pub(crate) fn enrich_schema(picker: &PickerData) -> Value {
    let mut schema = crate::schema::json_schema();
    let engine_names: Vec<Value> = picker.engines.keys().map(|k| json!(k)).collect();
    if let Some(roles_val) = schema.pointer_mut(
        "/properties/options/properties/proj.roles/properties/value/additionalProperties",
    ) {
        if let Some(eng) = roles_val.pointer_mut("/properties/engine") {
            eng["enum"] = Value::Array(engine_names.clone());
        }
        if let Some(hop_eng) = roles_val.pointer_mut("/properties/over/items/properties/engine") {
            hop_eng["enum"] = Value::Array(engine_names.clone());
        }
    }
    let mut engine_choices = Map::new();
    let mut job_variants = Vec::new();
    for (name, eng) in &picker.engines {
        let model_ids: Vec<Value> = eng.models.iter().map(|m| json!(m.id)).collect();
        let efforts: Vec<Value> = eng.efforts.iter().map(|e| json!(e)).collect();
        let model_schema = if model_ids.is_empty() {
            json!({"type": "null"})
        } else {
            json!({"anyOf": [{"type": "null"}, {"enum": model_ids}]})
        };
        let effort_schema = if efforts.is_empty() {
            json!({"type": "null"})
        } else {
            json!({"anyOf": [{"type": "null"}, {"enum": efforts}]})
        };
        let mut variant = json!({
            "type": "object",
            "properties": {
                "engine": {"const": name},
                "model": model_schema,
                "effort": effort_schema,
            },
            "required": ["engine"],
            "additionalProperties": false,
            "x-models": eng.models,
            "x-efforts": eng.efforts,
            "x-installed": eng.installed,
        });
        if let Some(caps) = &eng.capabilities {
            variant["x-capabilities"] = json!(caps);
        }
        if !eng.meters.is_empty() {
            variant["x-meters"] = json!(eng.meters);
        }
        if let Some(version) = &eng.version {
            variant["x-version"] = json!(version);
        }
        if let Some(sha256) = &eng.sha256 {
            variant["x-sha256"] = json!(sha256);
        }
        // Preserve per-model effort pairs for installed plugins so JSON Schema
        // rejects cross-model effort combinations that `resolve_model_effort`
        // would refuse at turn start.
        if eng.installed {
            let null_or_enum = |items: &[String]| {
                if items.is_empty() {
                    json!({"type": "null"})
                } else {
                    json!({"anyOf": [{"type": "null"}, {"enum": items}]})
                }
            };
            let default_effort = if eng.models.len() == 1 {
                null_or_enum(&eng.models[0].efforts)
            } else {
                json!({"type": "null"})
            };
            let mut pairs = vec![json!({
                "properties": {
                    "model": {"type": "null"},
                    "effort": default_effort,
                }
            })];
            for model in &eng.models {
                pairs.push(json!({
                    "required": ["model"],
                    "properties": {
                        "model": {"const": model.id},
                        "effort": null_or_enum(&model.efforts),
                    }
                }));
            }
            variant["anyOf"] = Value::Array(pairs);
        }
        job_variants.push(variant);
        engine_choices.insert(name.clone(), json!(eng));
    }
    if let Some(models_opt) = schema.pointer_mut("/properties/options/properties/proj.models") {
        models_opt["x-engine-choices"] = Value::Object(engine_choices);
    }
    schema["$defs"] = json!({
        "jobChoice": {
            "description": "Valid per-job engine, model, and effort choice from currently configured and installed engine plugins.",
            "oneOf": job_variants,
        }
    });
    schema["x-picker"] = serde_json::to_value(picker).expect("picker is JSON");
    schema
}
