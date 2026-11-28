//! Which engine, model and effort runs each role: live settings in the
//! overlay, written through the door ([`crate::Config::command`]) and logged.
//! The schema declares each one's default ([`crate::schema`]); the overlay
//! holds what's set over it. Saved engine setups are named sets of these
//! same values, so a setup can't disagree with the settings in force.
//!
//! Model ids live only here, in the overlay: never in a tracked file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub(crate) mod store;
pub use store::{Migration, View, legacy_file};

/// Retired engines whose legacy records are never routed or re-offered.
pub const RETIRED_ENGINES: &[&str] = &["codex"];

/// Whether `name` has valid engine-plugin syntax (`^[a-z0-9][a-z0-9-]*$`).
pub fn valid_engine_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    (first.is_ascii_lowercase() || first.is_ascii_digit())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `name` is a retired engine that may never be routed or offered.
pub fn retired_engine(name: &str) -> bool {
    RETIRED_ENGINES.contains(&name)
}

/// Where a role's turns go: its engine, then, in order, the engines it
/// moves on to when the one before is short (`ENGINE over PCT ENGINE ...`).
/// A route with no `over` is stored as it always was.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Route {
    pub engine: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub over: Vec<Hop>,
}

/// One step along a route: once the engine before it has used `pct`
/// percent by its meter, the turn may move on to `engine`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Hop {
    pub pct: f64,
    pub engine: String,
}

/// A role's model and effort on one engine; null is the engine's own.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Tuning {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// What an engine offers on the machine: the models and efforts a role may
/// be set to on it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Offer {
    pub models: Vec<String>,
    pub efforts: Vec<String>,
}

/// The role settings set over the schema's defaults: each role key's route
/// (`ROLE[@OWNER/REPO]`, or `*` for every role), and each role key's tuning
/// on an engine (`KEY ENGINE`). A saved engine setup is one of these.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Roles {
    #[serde(default)]
    pub roles: BTreeMap<String, Route>,
    #[serde(default)]
    pub models: BTreeMap<String, Tuning>,
}

/// The saved setups, the one put in force last, and what was in force before
/// it (`engines use back`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Setups {
    #[serde(default)]
    pub saved: BTreeMap<String, Roles>,
    #[serde(default)]
    pub active: Option<String>,
    #[serde(default)]
    pub previous: Option<Roles>,
}

/// A role key: a role name, or `*` for every role, optionally in one repo.
/// Only the repo folds case.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key<'a> {
    name: &'a str,
    repo: Option<String>,
}

impl<'a> Key<'a> {
    pub fn parse(key: &'a str) -> Result<Self, String> {
        let (name, repo) = key
            .split_once('@')
            .map_or((key, None), |(name, repo)| (name, Some(repo)));
        if !word(name) || repo.is_some_and(|r| !burrow_github::repo_name(r)) {
            return Err(format!(
                "{key}: expected ROLE, *, ROLE@OWNER/REPO or *@OWNER/REPO"
            ));
        }
        Ok(Self {
            name,
            repo: repo.map(str::to_ascii_lowercase),
        })
    }

    /// How closely it matches ROLE in REPO, higher is closer: a named role
    /// beats `*`, and a repo's key beats the bare one. None is no match.
    pub fn specificity(&self, role: &str, repo: Option<&str>) -> Option<u8> {
        let named = match self.name {
            "*" => 0,
            name if name == role => 2,
            _ => return None,
        };
        match &self.repo {
            None => Some(named),
            Some(scope) if repo.is_some_and(|r| scope.eq_ignore_ascii_case(r)) => Some(named + 1),
            Some(_) => None,
        }
    }

    /// Its spelling as a map key, its repo folded.
    pub fn canonical(&self) -> String {
        match &self.repo {
            Some(repo) => format!("{}@{repo}", self.name),
            None => self.name.to_owned(),
        }
    }
}

fn word(s: &str) -> bool {
    !s.is_empty() && !s.contains(char::is_whitespace) && !s.contains('@')
}

fn engine_syntax(name: &str) -> Result<(), String> {
    if valid_engine_name(name) && !retired_engine(name) {
        Ok(())
    } else {
        Err(format!("unknown engine {name}"))
    }
}

fn known_engine(name: &str, offers: &BTreeMap<String, Offer>) -> Result<(), String> {
    engine_syntax(name)?;
    if offers.contains_key(name) {
        Ok(())
    } else {
        Err(format!("unknown engine {name}"))
    }
}

fn offerable_engine(name: &str, configured: &[String]) -> Result<(), String> {
    engine_syntax(name)?;
    if configured.is_empty() || configured.iter().any(|e| e == name) {
        Ok(())
    } else {
        Err(format!("unknown engine {name}"))
    }
}

/// A model id or effort level as the engines' CLIs take one.
pub fn plain(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:[]-".contains(&b))
}

/// The best match for ROLE in REPO among role keys, by [`Key::specificity`].
pub fn best<'v, V: 'v>(
    entries: impl IntoIterator<Item = (&'v str, &'v V)>,
    role: &str,
    repo: Option<&str>,
) -> Option<&'v V> {
    entries
        .into_iter()
        .filter_map(|(k, v)| {
            Key::parse(k)
                .ok()?
                .specificity(role, repo)
                .map(|rank| (rank, v))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, v)| v)
}

/// The settings in force: the schema's defaults with the overlay's values
/// set over them, and what each engine offers.
#[derive(Clone, Debug, PartialEq)]
pub struct InForce {
    pub roles: BTreeMap<String, Route>,
    pub models: BTreeMap<String, Tuning>,
    pub offers: BTreeMap<String, Offer>,
    /// Per-model effort catalogs for installed plugins whose offers come
    /// directly from their validated `describe` answer.
    pub plugin_models: BTreeMap<String, Vec<crate::plugins::PluginModel>>,
    /// Fallback engines recorded when installed plugins were removed, so
    /// persisted job pins fall back cleanly instead of failing.
    pub removed_fallbacks: BTreeMap<String, String>,
}

impl InForce {
    /// The route ROLE in REPO takes. The schema's `*` always matches.
    pub fn route(&self, role: &str, repo: Option<&str>) -> &Route {
        best(self.roles.iter().map(|(k, v)| (k.as_str(), v)), role, repo)
            .expect("the schema's default routes every role")
    }

    /// ROLE's model and effort on ENGINE in REPO.
    pub fn tuning(&self, role: &str, repo: Option<&str>, engine: &str) -> Tuning {
        best(
            self.models.iter().filter_map(|(k, v)| {
                k.split_once(' ')
                    .filter(|(_, on)| *on == engine)
                    .map(|(key, _)| (key, v))
            }),
            role,
            repo,
        )
        .cloned()
        .unwrap_or_default()
    }

    /// Validate `(model, effort)` for `engine`, preserving per-model effort
    /// catalogs declared by installed plugins.
    pub fn validate_choice(
        &self,
        engine: &str,
        model: Option<&str>,
        effort: Option<&str>,
    ) -> Result<(), String> {
        validate_choice_against(engine, model, effort, &self.offers, &self.plugin_models)
    }

    /// When `engine` is no longer active because its installed plugin was
    /// removed, return the declared fallback engine recorded at removal.
    pub fn fallback_for(&self, engine: &str) -> Option<&str> {
        if self.offers.contains_key(engine) {
            return None;
        }
        self.removed_fallbacks.get(engine).map(String::as_str)
    }
}

/// Per-model catalogs for installed plugins whose offers were not manually
/// overridden in `offers`.
pub(crate) fn plugin_models_for(
    offers: &BTreeMap<String, Offer>,
    plugins: &BTreeMap<String, crate::plugins::InstalledPlugin>,
) -> BTreeMap<String, Vec<crate::plugins::PluginModel>> {
    plugins
        .iter()
        .filter(|(name, _)| !offers.contains_key(*name))
        .map(|(name, plugin)| (name.clone(), plugin.models.clone()))
        .collect()
}

/// Validate `(model, effort)` for `engine` against `offers` and any per-model
/// effort catalog in `plugin_models`, matching `conary-plugin`'s
/// `Offers::allows` and `burrow-proj`'s `resolve_model_effort`.
pub(crate) fn validate_choice_against(
    engine: &str,
    model: Option<&str>,
    effort: Option<&str>,
    offers: &BTreeMap<String, Offer>,
    plugin_models: &BTreeMap<String, Vec<crate::plugins::PluginModel>>,
) -> Result<(), String> {
    known_engine(engine, offers)?;
    if let Some(m) = model
        && !plain(m)
    {
        return Err(format!("{engine} doesn't offer model {m:?}"));
    }
    if let Some(e) = effort
        && !plain(e)
    {
        return Err(format!("{engine} doesn't offer effort {e}"));
    }
    if let Some(models) = plugin_models.get(engine) {
        return match (model, effort) {
            (None, None) => Ok(()),
            (Some(m), None) => {
                if models.iter().any(|pm| pm.id == m) {
                    Ok(())
                } else {
                    Err(format!("{engine} doesn't offer that model ({m:?})"))
                }
            }
            (Some(m), Some(e)) => {
                let Some(pm) = models.iter().find(|pm| pm.id == m) else {
                    return Err(format!("{engine} doesn't offer that model ({m:?})"));
                };
                if pm.efforts.iter().any(|ef| ef == e) {
                    Ok(())
                } else {
                    Err(format!(
                        "{engine} doesn't offer model {m:?} with effort {e:?}"
                    ))
                }
            }
            (None, Some(e)) => {
                if models.len() == 1 {
                    if models[0].efforts.iter().any(|ef| ef == e) {
                        Ok(())
                    } else {
                        Err(format!("{engine} doesn't offer effort {e}"))
                    }
                } else {
                    Err(format!("{engine} requires a model to select effort {e:?}"))
                }
            }
        };
    }
    let offer = offers.get(engine).cloned().unwrap_or_default();
    if let Some(m) = model
        && !offer.models.iter().any(|x| x == m)
    {
        return Err(format!("{engine} doesn't offer that model"));
    }
    if let Some(e) = effort
        && !offer.efforts.iter().any(|x| x == e)
    {
        return Err(format!("{engine} doesn't offer effort {e}"));
    }
    Ok(())
}

fn declared<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let value = crate::schema::find(name)
        .and_then(crate::schema::Setting::default_value)
        .unwrap_or_else(|| panic!("{name}: the schema declares no default"));
    serde_json::from_value(value).unwrap_or_else(|e| panic!("{name}: its default: {e}"))
}

/// The schema's default routes (`proj.roles`).
pub fn default_routes() -> BTreeMap<String, Route> {
    declared("proj.roles")
}

/// The schema's default tunings (`proj.models`).
pub fn default_models() -> BTreeMap<String, Tuning> {
    declared("proj.models")
}

/// The schema's default offers (`proj.engineOffers`).
pub fn default_offers() -> BTreeMap<String, Offer> {
    declared("proj.engineOffers")
}

/// Default offers seeded with any additional configured engine plugins.
pub fn default_offers_for(configured: &[String]) -> BTreeMap<String, Offer> {
    let mut offers = default_offers();
    for name in configured {
        if valid_engine_name(name) && !retired_engine(name) {
            offers.entry(name.clone()).or_default();
        }
    }
    offers
}

/// Merge persisted offers over the machine's default offers and any live
/// installed engine plugins. Every persisted entry is checked for valid syntax
/// and plain words; when `configured` is non-empty, only engines present in
/// `configured` or `plugins` remain routable in the merged offer map, so
/// removing a plugin invalidates any route or tuning still targeting it while
/// letting operators clear those references through the settings door.
pub(crate) fn merge_offers_with_plugins(
    offers: &BTreeMap<String, Offer>,
    configured: &[String],
    plugins: &BTreeMap<String, crate::plugins::InstalledPlugin>,
) -> Result<BTreeMap<String, Offer>, String> {
    let mut offered = default_offers_for(configured);
    for (name, plugin) in plugins {
        engine_syntax(name)?;
        offered.insert(name.clone(), plugin.offer());
    }
    for (name, offer) in offers {
        engine_syntax(name)?;
        for word in offer.models.iter().chain(&offer.efforts) {
            if !plain(word) {
                return Err(format!("{name}: offers {word:?}, not a plain word"));
            }
        }
        if offerable_engine(name, configured).is_ok() || plugins.contains_key(name) {
            offered.insert(name.clone(), offer.clone());
        }
    }
    Ok(offered)
}

impl Roles {
    /// These values over the schema's defaults, each checked against the
    /// offers.
    pub fn in_force(&self, offers: &BTreeMap<String, Offer>) -> Result<InForce, String> {
        self.in_force_for(offers, &[])
    }

    /// [`Self::in_force`], seeded with the machine's configured engine plugins.
    pub fn in_force_for(
        &self,
        offers: &BTreeMap<String, Offer>,
        configured: &[String],
    ) -> Result<InForce, String> {
        self.in_force_with_plugins(offers, configured, &BTreeMap::new(), &BTreeMap::new())
    }

    /// [`Self::in_force_for`], also seeding live installed engine plugins and
    /// removed-plugin fallbacks.
    pub(crate) fn in_force_with_plugins(
        &self,
        offers: &BTreeMap<String, Offer>,
        configured: &[String],
        plugins: &BTreeMap<String, crate::plugins::InstalledPlugin>,
        removed_fallbacks: &BTreeMap<String, String>,
    ) -> Result<InForce, String> {
        let offered = merge_offers_with_plugins(offers, configured, plugins)?;
        let plugin_models = plugin_models_for(offers, plugins);
        self.check_with_plugins(&offered, &plugin_models)?;
        for (removed, fallback) in removed_fallbacks {
            engine_syntax(removed)?;
            known_engine(fallback, &offered)
                .map_err(|e| format!("removed fallback {removed} -> {fallback}: {e}"))?;
        }
        let mut roles = default_routes();
        roles.extend(self.roles.clone());
        let mut models = default_models();
        for name in configured.iter().chain(plugins.keys()) {
            if valid_engine_name(name) && !retired_engine(name) {
                models.entry(format!("* {name}")).or_default();
            }
        }
        models.extend(self.models.clone());
        Ok(InForce {
            roles,
            models,
            offers: offered,
            plugin_models,
            removed_fallbacks: removed_fallbacks.clone(),
        })
    }

    /// Every key and value well formed, every engine known, every model and
    /// effort one its engine offers.
    pub fn check(&self, offers: &BTreeMap<String, Offer>) -> Result<(), String> {
        self.check_with_plugins(offers, &BTreeMap::new())
    }

    /// [`Self::check`], enforcing per-model effort pairs for installed plugins.
    pub(crate) fn check_with_plugins(
        &self,
        offers: &BTreeMap<String, Offer>,
        plugin_models: &BTreeMap<String, Vec<crate::plugins::PluginModel>>,
    ) -> Result<(), String> {
        for (key, route) in &self.roles {
            canonical(key)?;
            route
                .check_against(offers)
                .map_err(|e| format!("role {key}: {e}"))?;
        }
        for (key, tuning) in &self.models {
            let Some((role, on)) = key.split_once(' ') else {
                return Err(format!("model {key}: expected ROLE ENGINE"));
            };
            canonical(role)?;
            validate_choice_against(
                on,
                tuning.model.as_deref(),
                tuning.effort.as_deref(),
                offers,
                plugin_models,
            )
            .map_err(|e| format!("model {key}: {e}"))?;
        }
        Ok(())
    }
}

/// A key as it's spelled where it's stored; an error for one that isn't.
fn canonical(key: &str) -> Result<String, String> {
    let canonical = Key::parse(key)?.canonical();
    if canonical != key {
        return Err(format!("{key}: stored as {canonical}"));
    }
    Ok(canonical)
}

/// The engine words of a route: `ENGINE [over PCT ENGINE]...`.
pub fn route(words: &[&str]) -> Result<Route, String> {
    const EXPECTED: &str = "expected ENGINE [over PCT ENGINE]...";
    let [first, rest @ ..] = words else {
        return Err(EXPECTED.into());
    };
    if rest.len() % 3 != 0 {
        return Err(EXPECTED.into());
    }
    let over = rest
        .chunks(3)
        .map(|hop| match hop {
            ["over", pct, e] => Ok(Hop {
                pct: pct
                    .parse()
                    .ok()
                    .filter(|p: &f64| p.is_finite())
                    .ok_or("over: PCT must be a number from 0 to 100")?,
                engine: (*e).into(),
            }),
            _ => Err(EXPECTED.to_owned()),
        })
        .collect::<Result<_, _>>()?;
    let route = Route {
        engine: (*first).into(),
        over,
    };
    route.check()?;
    Ok(route)
}

impl Route {
    /// As a roles line said it: `ENGINE [over PCT ENGINE]...`.
    pub fn text(&self) -> String {
        let mut text = self.engine.clone();
        for hop in &self.over {
            text += &format!(" over {} {}", hop.pct, hop.engine);
        }
        text
    }

    /// Its engines in order: the first, then each it may move on to.
    pub fn engines(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.engine.as_str()).chain(self.over.iter().map(|h| h.engine.as_str()))
    }

    /// Every engine valid and named once, every percent from 0 to 100.
    pub(crate) fn check(&self) -> Result<(), String> {
        let mut seen = Vec::new();
        for name in self.engines() {
            engine_syntax(name)?;
            if seen.contains(&name) {
                return Err(format!("{name} is on the route twice"));
            }
            seen.push(name);
        }
        if let Some(hop) = self.over.iter().find(|h| !(0.0..=100.0).contains(&h.pct)) {
            return Err(format!(
                "over {} {}: PCT must be a number from 0 to 100",
                hop.pct, hop.engine
            ));
        }
        Ok(())
    }

    /// [`Self::check`], also verifying every engine on the route is configured
    /// or offered on this machine.
    fn check_against(&self, offers: &BTreeMap<String, Offer>) -> Result<(), String> {
        self.check()?;
        for name in self.engines() {
            known_engine(name, offers)?;
        }
        Ok(())
    }
}

pub fn valid_setup(name: &str) -> bool {
    !name.is_empty()
        && name != "back"
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// One change to the role settings, applied to the overlay's values. Each is
/// absolute, so repeating it changes nothing more. Its answer says what's
/// in force after it.
pub(crate) fn apply(
    roles: &mut Roles,
    offers: &mut BTreeMap<String, Offer>,
    setups: &mut Setups,
    configured_engines: &[String],
    plugins: &BTreeMap<String, crate::plugins::InstalledPlugin>,
    args: &[&str],
) -> Result<String, String> {
    let before = active(roles, setups);
    let offered = |offers: &BTreeMap<String, Offer>| {
        merge_offers_with_plugins(offers, configured_engines, plugins)
    };
    match args {
        ["role", key, "unset"] => {
            roles.roles.remove(&Key::parse(key)?.canonical());
        }
        ["role", key, words @ ..] => {
            let parsed = route(words)?;
            parsed.check_against(&offered(offers)?)?;
            roles.roles.insert(Key::parse(key)?.canonical(), parsed);
        }
        ["model", key, on, "unset"] => {
            let model_key = format!("{} {on}", Key::parse(key)?.canonical());
            if !roles.models.contains_key(&model_key) {
                known_engine(on, &offered(offers)?)?;
            }
            roles.models.remove(&model_key);
        }
        ["model", key, on, model, effort @ ..] => {
            let all_offers = offered(offers)?;
            known_engine(on, &all_offers)?;
            let effort = match effort {
                [] | ["-"] => None,
                [e] => Some((*e).to_owned()),
                _ => return Err("expected model ROLE ENGINE MODEL|- [EFFORT|-]".into()),
            };
            let tuning = Tuning {
                model: (*model != "-").then(|| (*model).to_owned()),
                effort,
            };
            let key = format!("{} {on}", Key::parse(key)?.canonical());
            let mut next = roles.clone();
            next.models.insert(key, tuning);
            let pm = plugin_models_for(offers, plugins);
            next.check_with_plugins(&all_offers, &pm)?;
            *roles = next;
        }
        ["offer", on, what @ ("models" | "efforts"), words @ ..] => {
            apply_offer(roles, offers, configured_engines, plugins, on, what, words)?;
        }
        ["engines", "save", name, rest @ ..] => {
            if !valid_setup(name) {
                return Err("setup name must be [a-z0-9-]+ and not back".into());
            }
            match rest {
                [] if setups.saved.contains_key(*name) => {
                    return Err(format!("setup {name} exists; save {name} --replace"));
                }
                [] | ["--replace"] => {}
                _ => return Err("usage: engines save NAME [--replace]".into()),
            }
            setups.saved.insert((*name).to_owned(), roles.clone());
            setups.active = Some((*name).to_owned());
        }
        ["engines", "use", name] => {
            let next = if *name == "back" {
                setups.previous.clone().ok_or("no previous setup")?
            } else {
                setups
                    .saved
                    .get(*name)
                    .cloned()
                    .ok_or_else(|| format!("unknown setup: {name}"))?
            };
            let pm = plugin_models_for(offers, plugins);
            next.check_with_plugins(&offered(offers)?, &pm)?;
            let was = std::mem::replace(roles, next);
            if was != *roles {
                setups.previous = Some(was);
            }
            setups.active = (*name != "back")
                .then(|| (*name).to_owned())
                .or_else(|| active(roles, setups));
        }
        _ => return Err(USAGE.into()),
    }
    if args[0] != "engines" && active(roles, setups).is_none() {
        setups.active = None;
    }
    Ok(status(before.as_deref(), roles, setups))
}

fn apply_offer(
    roles: &Roles,
    offers: &mut BTreeMap<String, Offer>,
    configured_engines: &[String],
    plugins: &BTreeMap<String, crate::plugins::InstalledPlugin>,
    on: &str,
    what: &str,
    words: &[&str],
) -> Result<(), String> {
    if !plugins.contains_key(on) {
        offerable_engine(on, configured_engines)?;
    } else {
        engine_syntax(on)?;
    }
    if let Some(w) = words.iter().find(|w| !plain(w)) {
        return Err(format!("{w:?} is not a plain word"));
    }
    let mut next = merge_offers_with_plugins(offers, configured_engines, plugins)?;
    let offer = next.entry(on.to_owned()).or_default();
    let list: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
    if what == "models" {
        offer.models = list;
    } else {
        offer.efforts = list;
    }
    let mut stored = offers.clone();
    stored.insert(on.to_owned(), next[on].clone());
    // Withdrawing what a role is set to would leave it on a model
    // its engine doesn't have.
    let pm = plugin_models_for(&stored, plugins);
    roles.check_with_plugins(&next, &pm)?;
    *offers = stored;
    Ok(())
}

pub(crate) const USAGE: &str = "usage: settings role ROLE ENGINE [over PCT ENGINE]... | role ROLE unset | model ROLE ENGINE MODEL|- [EFFORT|-] | model ROLE ENGINE unset | offer ENGINE models|efforts WORD... | engines [save NAME [--replace] | use NAME | use back]";

/// The setup whose values are the ones in force: the last one put in force
/// while it still is, else any equal one.
pub(crate) fn active(roles: &Roles, setups: &Setups) -> Option<String> {
    setups
        .active
        .iter()
        .chain(setups.saved.keys())
        .find(|name| setups.saved.get(*name) == Some(roles))
        .cloned()
}

/// What `engines` says: the setups, which is active, and each role's route
/// over the defaults. No model id: an issue door answers with it.
pub(crate) fn status(before: Option<&str>, roles: &Roles, setups: &Setups) -> String {
    let active = active(roles, setups);
    let previous = setups.previous.as_ref().map(|p| {
        setups
            .saved
            .iter()
            .find(|(_, s)| *s == p)
            .map_or("unsaved", |(n, _)| n.as_str())
    });
    let mut out = format!(
        "Was active: {}\nActive: {}\nPrevious: {}\n",
        before.unwrap_or("unsaved"),
        active.as_deref().unwrap_or("unsaved"),
        previous.unwrap_or("none")
    );
    for name in setups.saved.keys() {
        let mark = if active.as_deref() == Some(name) {
            "*"
        } else {
            " "
        };
        out += &format!("{mark} {name}\n");
    }
    let mut routes = default_routes();
    routes.extend(roles.roles.clone());
    for (key, route) in routes {
        out += &format!("{key} {}\n", route.text());
    }
    out
}

#[cfg(test)]
mod tests;
