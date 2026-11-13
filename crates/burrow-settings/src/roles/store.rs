//! Where the role settings live: a document of their own beside the
//! admission overlay (`overlay.roles.json`), under the same lock and log, so
//! a generation that predates them still reads its overlay after a
//! rollback.

use super::{InForce, Offer, Roles, Setups, active, apply, default_offers, status, valid_setup};
use crate::{Config, live, now};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Document {
    #[serde(default)]
    receipts: BTreeMap<String, String>,
    /// Which engine, model and effort runs each role, over the schema's
    /// defaults.
    #[serde(default)]
    routing: Roles,
    /// What each engine offers, where it isn't the schema's default.
    #[serde(default)]
    offers: BTreeMap<String, Offer>,
    #[serde(default)]
    setups: Setups,
    /// When proj's old roles and models files were moved in.
    #[serde(default)]
    migrated: Option<i64>,
}

impl live::Document for Document {
    fn receipts(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.receipts
    }
}

/// proj's old roles, models and setups files, read into the settings once
/// ([`Config::migrate`]).
#[derive(Debug, Default)]
pub struct Migration {
    pub routing: Roles,
    /// Models and efforts the old files set, which the engines offered.
    pub offers: BTreeMap<String, Offer>,
    pub setups: Setups,
    /// Replay protection for a recovered legacy request, published with routing.
    pub receipts: BTreeMap<String, String>,
}

/// The role settings, all from one version of their document.
#[derive(Debug)]
pub struct View {
    /// The declared defaults with what's set over them.
    pub in_force: InForce,
    /// Whether routes, tunings and offers are set over the defaults.
    pub set: (bool, bool, bool),
    /// The saved setups and the one in force, as `settings show` gives
    /// them; None when none is saved.
    pub setups: Option<serde_json::Value>,
    /// Whether proj's old files were moved in.
    pub migrated: bool,
    pub file: PathBuf,
}

/// The first retained legacy input, with lookup failures propagated. Both
/// the settings door and proj use this gate; admission never calls it.
pub fn legacy_file(dir: &Path) -> Result<Option<PathBuf>, String> {
    for name in [
        "roles",
        "models",
        "roles.d",
        "roles.previous",
        "roles.operation",
    ] {
        let path = dir.join(name);
        if live::path_exists(&path)? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

impl Config {
    /// The file the role settings are kept in.
    pub fn roles_file(&self) -> PathBuf {
        self.overlay.with_extension("roles.json")
    }

    fn roles_doc(&self) -> Result<Document, String> {
        live::read(&self.roles_file())
    }

    /// Whether migration was published, without attempting to consume roles.
    pub fn roles_migrated(&self) -> Result<bool, String> {
        Ok(self.roles_doc()?.migrated.is_some())
    }

    fn ready(&self, doc: &Document, mutation: bool) -> Result<(), String> {
        if doc.migrated.is_none()
            && let Some(dir) = &self.legacy_roles
        {
            if mutation {
                return Err("role settings await proj settings migrate".into());
            }
            if legacy_file(dir)?.is_some() {
                return Err("proj's old role settings aren't moved into the settings yet (proj settings migrate)".into());
            }
        }
        Ok(())
    }

    /// The role settings, read once and checked again: a document a hand
    /// edit broke fails the turn rather than running a default.
    pub fn role_view(&self) -> Result<View, String> {
        let doc = self.roles_doc()?;
        self.ready(&doc, false)?;
        let in_force = doc
            .routing
            .in_force(&doc.offers, self.claude_effort.as_deref())?;
        Ok(View {
            in_force,
            set: (
                !doc.routing.roles.is_empty(),
                !doc.routing.models.is_empty(),
                !doc.offers.is_empty(),
            ),
            setups: (!doc.setups.saved.is_empty()).then(|| {
                serde_json::json!({
                    "active": active(&doc.routing, &doc.setups),
                    "saved": doc.setups.saved,
                })
            }),
            migrated: doc.migrated.is_some(),
            file: self.roles_file(),
        })
    }

    /// The role settings in force.
    pub fn roles(&self) -> Result<InForce, String> {
        Ok(self.role_view()?.in_force)
    }

    /// `engines`: the setups and the routes in force, changing nothing.
    pub fn engines(&self) -> Result<String, String> {
        let doc = self.roles_doc()?;
        self.ready(&doc, false)?;
        doc.routing
            .in_force(&doc.offers, self.claude_effort.as_deref())?;
        let active = active(&doc.routing, &doc.setups);
        Ok(status(active.as_deref(), &doc.routing, &doc.setups))
    }

    /// What the request with RECEIPT was answered, once its change is
    /// published (not necessarily durable: [`Config::request`] says that).
    pub fn answered(&self, receipt: &Path) -> Result<Option<String>, String> {
        Ok(self
            .roles_doc()?
            .receipts
            .get(&crate::receipt_key(receipt)?)
            .cloned())
    }

    /// A legacy completed answer, imported once after cutover. The receipt
    /// then survives loss of the legacy file, and retries sync the publication
    /// before returning it, just like a new request.
    pub fn legacy_answer(
        &self,
        receipt: &Path,
        answer: Option<&str>,
    ) -> Result<Option<String>, String> {
        let receipt = crate::receipt_key(receipt)?;
        live::write(
            &self.roles_file(),
            &self.overlay.with_extension("lock"),
            &self.overlay.with_extension("log"),
            "legacy engines door",
            Some(&receipt),
            &["legacy receipt".into()],
            |doc: &mut Document| {
                self.ready(doc, true)?;
                Ok(answer.map(str::to_owned))
            },
        )
    }

    pub(crate) fn change_roles(
        &self,
        words: &[&str],
        args: &[String],
        actor: &str,
        receipt: Option<&str>,
    ) -> Result<String, String> {
        live::write(
            &self.roles_file(),
            &self.overlay.with_extension("lock"),
            &self.overlay.with_extension("log"),
            actor,
            receipt,
            args,
            |doc: &mut Document| {
                self.ready(doc, true)?;
                let answer = apply(&mut doc.routing, &mut doc.offers, &mut doc.setups, words)?;
                // With no migration source configured this is a fresh store.
                // Its first accepted mutation establishes the complete state;
                // a later import must not resurrect values it deliberately lacks.
                doc.migrated.get_or_insert_with(now);
                Ok(Some(answer))
            },
        )
        .map(Option::unwrap_or_default)
    }

    /// Moves proj's old files in, once: false, changing nothing, when they
    /// were moved already. Configured legacy inputs gate every mutation until
    /// this publication; a store with no legacy source starts from defaults.
    pub fn migrate(&self, from: Migration, actor: &str) -> Result<bool, String> {
        let mut moved = false;
        live::write(
            &self.roles_file(),
            &self.overlay.with_extension("lock"),
            &self.overlay.with_extension("log"),
            actor,
            None,
            &["migrate".into()],
            |doc: &mut Document| {
                if doc.migrated.is_some() {
                    return Ok(None);
                }
                let mut offers = default_offers();
                for (engine, offer) in from.offers {
                    let have = offers.entry(engine).or_default();
                    for m in offer.models {
                        if !have.models.contains(&m) {
                            have.models.push(m);
                        }
                    }
                    for e in offer.efforts {
                        if !have.efforts.contains(&e) {
                            have.efforts.push(e);
                        }
                    }
                }
                let routing = from.routing;
                routing.in_force(&offers, self.claude_effort.as_deref())?;
                let setups = from.setups;
                for (name, setup) in &setups.saved {
                    if !valid_setup(name) {
                        return Err(format!("setup {name}: not a setup name"));
                    }
                    setup
                        .check(&offers)
                        .map_err(|e| format!("setup {name}: {e}"))?;
                }
                // Only what differs from the declared default is set.
                let declared = default_offers();
                offers.retain(|engine, offer| declared.get(engine) != Some(offer));
                for (receipt, answer) in from.receipts {
                    let key = crate::receipt_key(Path::new(&receipt))?;
                    if doc.receipts.get(&key).is_some_and(|old| old != &answer) {
                        return Err("conflicting answers for the same receipt".into());
                    }
                    doc.receipts.insert(key, answer);
                }
                doc.offers = offers;
                doc.routing = routing;
                doc.setups = setups;
                doc.migrated = Some(now());
                moved = true;
                Ok(Some(
                    "Moved proj's roles and models into the settings.".into(),
                ))
            },
        )?;
        Ok(moved)
    }
}
