//! Which engine, model and effort runs each role: live settings in the
//! overlay, written through the door ([`crate::Config::command`]) and logged.
//! The schema declares each one's default ([`crate::schema`]); the overlay
//! holds what's set over it. Saved engine setups are named sets of these
//! same values, so a setup can't disagree with the settings in force.
//!
//! Model ids live only here, in the overlay: never in a tracked file.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod store;
pub use store::{Migration, View, legacy_file};

/// The engines a role can run on.
pub const ENGINES: &[&str] = &["claude", "codex"];

/// Where a role's turns go: its engine, and the engine it moves to when its
/// own is short by its meter (`over`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Route {
    pub engine: String,
    pub over: Option<Overflow>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Overflow {
    /// A meter window at this percent makes the engine short: any finite
    /// number from 0 to 100.
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

fn engine(name: &str) -> Result<(), String> {
    if ENGINES.contains(&name) {
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
}

impl InForce {
    /// The route ROLE in REPO takes. The schema's `*` always matches.
    pub fn route(&self, role: &str, repo: Option<&str>) -> &Route {
        best(self.roles.iter().map(|(k, v)| (k.as_str(), v)), role, repo)
            .expect("the schema's default routes every role")
    }

    /// ROLE's model and effort on ENGINE in REPO.
    pub fn tuning(&self, role: &str, repo: Option<&str>, engine: &str) -> Tuning {
        // The declared `* ENGINE` tuning matches every role, so one is found.
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
        .unwrap_or_else(|| panic!("the schema's default tunes every role on {engine}"))
    }
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

/// The roles whose engine is Claude by deliberate policy, and why: each is a
/// declared default route of `proj.roles` with no overflow, so no meter moves
/// it to Codex. `settings show` names them; a live `role` line changes one.
pub const CLAUDE_ONLY: &[(&str, &str)] = &[
    (
        "update",
        "a failed machine update is repaired with the machine's own Claude instructions and login, and a meter must not hand a system change to another engine",
    ),
    (
        "backup",
        "a backup repair touches the only copy off the box, so it stays on the engine that guards it and never spills to another",
    ),
    (
        "firmware",
        "a firmware review precedes flashing, which stays the owner's call, so it stays on Claude and never spills to another engine",
    ),
];

/// The schema's default tunings (`proj.models`), with START as `* claude`'s
/// effort where the flake gives the machine one (`burrow.claude.effort`): a
/// declared starting point, which a live `model` line replaces.
pub fn default_models(start: Option<&str>) -> BTreeMap<String, Tuning> {
    let mut models: BTreeMap<String, Tuning> = declared("proj.models");
    if let Some(start) = start {
        models
            .get_mut("* claude")
            .expect("the schema tunes every role on claude")
            .effort = Some(start.to_owned());
    }
    models
}

/// The schema's default offers (`proj.engineOffers`).
pub fn default_offers() -> BTreeMap<String, Offer> {
    declared("proj.engineOffers")
}

impl Roles {
    /// These values over the schema's defaults, each checked against the
    /// offers.
    pub fn in_force(
        &self,
        offers: &BTreeMap<String, Offer>,
        start: Option<&str>,
    ) -> Result<InForce, String> {
        let mut offered = default_offers();
        offered.extend(offers.clone());
        for (name, offer) in &offered {
            engine(name)?;
            for word in offer.models.iter().chain(&offer.efforts) {
                if !plain(word) {
                    return Err(format!("{name}: offers {word:?}, not a plain word"));
                }
            }
        }
        self.check(&offered)?;
        let mut roles = default_routes();
        roles.extend(self.roles.clone());
        let mut models = default_models(start);
        if let Some(start) = start
            && !offered["claude"].efforts.iter().any(|e| e == start)
        {
            return Err(format!("claude: its starting effort {start} isn't offered"));
        }
        models.extend(self.models.clone());
        Ok(InForce {
            roles,
            models,
            offers: offered,
        })
    }

    /// Every key and value well formed, every engine known, every model and
    /// effort one its engine offers.
    pub fn check(&self, offers: &BTreeMap<String, Offer>) -> Result<(), String> {
        for (key, route) in &self.roles {
            canonical(key)?;
            engine(&route.engine).map_err(|e| format!("role {key}: {e}"))?;
            if let Some(over) = &route.over {
                engine(&over.engine).map_err(|e| format!("role {key}: {e}"))?;
                if !(0.0..=100.0).contains(&over.pct) {
                    return Err(format!("role {key}: overflow percent must be 0 to 100"));
                }
            }
        }
        for (key, tuning) in &self.models {
            let Some((role, on)) = key.split_once(' ') else {
                return Err(format!("model {key}: expected ROLE ENGINE"));
            };
            canonical(role)?;
            engine(on).map_err(|e| format!("model {key}: {e}"))?;
            let offer = offers.get(on).cloned().unwrap_or_default();
            if let Some(model) = &tuning.model
                && !offer.models.contains(model)
            {
                return Err(format!("model {key}: {on} doesn't offer that model"));
            }
            if let Some(effort) = &tuning.effort
                && !offer.efforts.contains(effort)
            {
                return Err(format!("model {key}: {on} doesn't offer effort {effort}"));
            }
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

/// The engine words of a route: `ENGINE [over PCT ENGINE]`.
pub fn route(words: &[&str]) -> Result<Route, String> {
    let (primary, over) = match words {
        [e] => (*e, None),
        [e, "over", pct, other] => {
            let pct: f64 = pct
                .parse()
                .ok()
                .filter(|p| (0.0..=100.0).contains(p))
                .ok_or("overflow percent must be a number from 0 to 100")?;
            engine(other)?;
            (
                *e,
                Some(Overflow {
                    pct,
                    engine: (*other).into(),
                }),
            )
        }
        _ => return Err("expected ENGINE [over PCT ENGINE]".into()),
    };
    engine(primary)?;
    Ok(Route {
        engine: primary.into(),
        over,
    })
}

impl Route {
    /// As a roles line said it: `ENGINE [over PCT ENGINE]`.
    pub fn text(&self) -> String {
        match &self.over {
            Some(o) => format!("{} over {} {}", self.engine, o.pct, o.engine),
            None => self.engine.clone(),
        }
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
    args: &[&str],
) -> Result<String, String> {
    let before = active(roles, setups);
    let offered = |offers: &BTreeMap<String, Offer>| {
        let mut all = default_offers();
        all.extend(offers.clone());
        all
    };
    match args {
        ["role", key, "unset"] => {
            roles.roles.remove(&Key::parse(key)?.canonical());
        }
        ["role", key, words @ ..] => {
            roles
                .roles
                .insert(Key::parse(key)?.canonical(), route(words)?);
        }
        ["model", key, on, "unset"] => {
            engine(on)?;
            roles
                .models
                .remove(&format!("{} {on}", Key::parse(key)?.canonical()));
        }
        ["model", key, on, model, effort @ ..] => {
            engine(on)?;
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
            next.check(&offered(offers))?;
            *roles = next;
        }
        ["offer", on, what @ ("models" | "efforts"), words @ ..] => {
            engine(on)?;
            if let Some(w) = words.iter().find(|w| !plain(w)) {
                return Err(format!("{w:?} is not a plain word"));
            }
            let mut next = offered(offers);
            let offer = next.entry((*on).to_owned()).or_default();
            let list: Vec<String> = words.iter().map(|w| (*w).to_owned()).collect();
            if *what == "models" {
                offer.models = list;
            } else {
                offer.efforts = list;
            }
            // Withdrawing what a role is set to would leave it on a model
            // its engine doesn't have.
            roles.check(&next)?;
            let mut stored = offers.clone();
            stored.insert((*on).to_owned(), next[*on].clone());
            *offers = stored;
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
            next.check(&offered(offers))?;
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

pub(crate) const USAGE: &str = "usage: settings role ROLE ENGINE [over PCT ENGINE] | role ROLE unset | model ROLE ENGINE MODEL|- [EFFORT|-] | model ROLE ENGINE unset | offer ENGINE models|efforts WORD... | engines [save NAME [--replace] | use NAME | use back]";

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
