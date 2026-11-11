//! What `settings show` prints: each option's value in effect and where it
//! came from. A [`Document`] is only ever built from the schema's options:
//! a value read for a name the schema doesn't declare, a declared option
//! nothing read, a value of the wrong kind, or a default that isn't the
//! schema's fails it, so neither side can drift from the other unseen.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::schema::{self, SETTINGS, Setting};

/// Where a value came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// Nothing sets it: the schema's default.
    Default,
    /// The machine's flake sets it.
    Flake,
    /// A file on the machine that changes without a deploy.
    Live,
}

impl Origin {
    pub fn name(self) -> &'static str {
        match self {
            Origin::Default => "default",
            Origin::Flake => "flake",
            Origin::Live => "live",
        }
    }
}

/// One option's value, as its reader found it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Reading {
    #[serde(skip)]
    pub name: String,
    pub value: Value,
    pub origin: Origin,
    /// The live file it was read from.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
}

impl Reading {
    pub fn new(name: &str, value: Value, origin: Origin) -> Self {
        Self {
            name: name.into(),
            value,
            origin,
            file: None,
        }
    }

    /// One from a live file: the file's when it's there, the default when
    /// it isn't.
    pub fn live(name: &str, file: PathBuf, value: Option<Value>) -> Self {
        let origin = if value.is_some() {
            Origin::Live
        } else {
            Origin::Default
        };
        let value = value
            .or_else(|| schema::find(name).and_then(Setting::default_value))
            .unwrap_or(Value::Null);
        Self {
            name: name.into(),
            value,
            origin,
            file: Some(file),
        }
    }
}

/// The flake's own values, as modules/settings/tracked.nix renders them:
/// each option's name, then `{ "value": V, "set": BOOL, "default": D }` (D
/// its module's own default, given the machine's other values), or null
/// where the machine doesn't have the module that declares it (its default
/// holds). A module's default that isn't the schema's fails it, overridden
/// by the flake or not, a derived one's worked out from the values here.
pub fn flake(tracked: &Map<String, Value>) -> Result<Vec<Reading>, String> {
    let mut readings = Vec::new();
    let mut defaults = Vec::new();
    for (name, entry) in tracked {
        let setting = schema::find(name).ok_or_else(|| {
            format!("{name}: the flake renders an option the settings schema doesn't declare")
        })?;
        if entry.is_null() {
            let value = setting.default_value().ok_or_else(|| {
                format!("{name}: its default is derived, and the machine doesn't have its module")
            })?;
            readings.push(Reading::new(name, value, Origin::Default));
            continue;
        }
        let (Some(value), Some(Value::Bool(set)), Some(default)) =
            (entry.get("value"), entry.get("set"), entry.get("default"))
        else {
            return Err(format!(
                "{name}: expected {{\"value\": V, \"set\": BOOL, \"default\": D}}, got {entry}"
            ));
        };
        let origin = if *set { Origin::Flake } else { Origin::Default };
        readings.push(Reading::new(name, value.clone(), origin));
        defaults.push((setting, default));
    }
    let values = values(&readings);
    for (setting, default) in defaults {
        let want = setting.default_given(&values);
        if want.as_ref() != Some(default) {
            return Err(drift(setting.name, "its module's default", default, want));
        }
    }
    Ok(readings)
}

fn values(readings: &[Reading]) -> schema::Values {
    readings
        .iter()
        .map(|r| (r.name.clone(), r.value.clone()))
        .collect()
}

fn drift(name: &str, what: &str, got: &Value, want: Option<Value>) -> String {
    match want {
        Some(want) => format!("{name}: {what} is {got}, the settings schema's is {want}"),
        None => format!("{name}: {what} is {got}, and the settings schema can't work its own out"),
    }
}

/// Every option the schema declares, each with its reading.
#[derive(Debug, Serialize)]
pub struct Document {
    pub options: BTreeMap<String, Reading>,
}

impl Document {
    /// The document, or why these readings can't be one.
    pub fn build(readings: Vec<Reading>) -> Result<Self, String> {
        let values = values(&readings);
        let mut options = BTreeMap::new();
        for r in readings {
            let setting = schema::find(&r.name)
                .ok_or_else(|| format!("{}: not an option the settings schema declares", r.name))?;
            check(setting, &r, &values)?;
            if options.insert(r.name.clone(), r).is_some() {
                return Err(format!("{}: read twice", setting.name));
            }
        }
        if let Some(s) = SETTINGS.iter().find(|s| !options.contains_key(s.name)) {
            return Err(format!(
                "{}: declared in the settings schema, but nothing read it",
                s.name
            ));
        }
        Ok(Self { options })
    }

    pub fn json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a document is JSON") + "\n"
    }

    /// A line an option, in the schema's order: its name, its value, and
    /// where it came from.
    pub fn text(&self) -> String {
        let width = SETTINGS.iter().map(|s| s.name.len()).max().unwrap_or(0);
        SETTINGS
            .iter()
            .filter_map(|s| self.options.get(s.name).map(|r| (s, r)))
            .map(|(s, r)| {
                let from = match &r.file {
                    Some(file) if r.origin == Origin::Live => {
                        format!("live, {}", file.display())
                    }
                    _ => r.origin.name().to_owned(),
                };
                let change = match s.change {
                    schema::Change::Deploy => ", deploy to change",
                    schema::Change::Live => "",
                };
                format!("{:width$}  {}  ({from}{change})\n", s.name, r.value)
            })
            .collect()
    }
}

fn check(setting: &Setting, r: &Reading, values: &schema::Values) -> Result<(), String> {
    let name = setting.name;
    if !setting.origins().contains(&r.origin) {
        return Err(format!(
            "{name}: comes from {}, which a {} option can't",
            r.origin.name(),
            setting.change.name()
        ));
    }
    setting
        .kind
        .check(&r.value)
        .map_err(|e| format!("{name}: {e}"))?;
    let want = setting.default_given(values);
    if r.origin == Origin::Default && want.as_ref() != Some(&r.value) {
        return Err(drift(name, "its default where it lives", &r.value, want));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Every option at its default, as a machine with nothing set reads it:
    /// a derived one worked out from those before it.
    fn defaults() -> Vec<Reading> {
        let mut values = schema::Values::new();
        SETTINGS
            .iter()
            .map(|s| {
                let value = s.default_given(&values).unwrap();
                values.insert(s.name.into(), value.clone());
                Reading::new(s.name, value, Origin::Default)
            })
            .collect()
    }

    #[test]
    fn every_option_shown_is_one_the_schema_declares() {
        let doc = Document::build(defaults()).unwrap();
        let shown: Vec<_> = doc.options.keys().cloned().collect();
        let mut declared: Vec<_> = SETTINGS.iter().map(|s| s.name.to_owned()).collect();
        declared.sort();
        assert_eq!(shown, declared);
        assert_eq!(doc.text().lines().count(), SETTINGS.len());

        // One the schema doesn't declare fails it: the drift check's first rung.
        let mut extra = defaults();
        extra.push(Reading::new("proj.undeclared", json!(1), Origin::Flake));
        let err = Document::build(extra).unwrap_err();
        assert_eq!(
            err,
            "proj.undeclared: not an option the settings schema declares"
        );

        // So does one the schema declares that nothing read.
        let mut missing = defaults();
        missing.retain(|r| r.name != "autoland.rounds");
        let err = Document::build(missing).unwrap_err();
        assert!(err.starts_with("autoland.rounds: declared"), "{err}");
    }

    #[test]
    fn a_reading_is_held_to_its_option() {
        let with = |name: &str, r: Reading| {
            let mut all = defaults();
            all.retain(|o| o.name != name);
            all.push(r);
            Document::build(all)
        };
        // The flake's own value, of the right kind.
        assert!(
            with(
                "autoland.rounds",
                Reading::new("autoland.rounds", json!(3), Origin::Flake)
            )
            .is_ok()
        );
        let err = with(
            "autoland.rounds",
            Reading::new("autoland.rounds", json!("3"), Origin::Flake),
        )
        .unwrap_err();
        assert!(err.starts_with("autoland.rounds: \"3\" is not"), "{err}");
        // A default that isn't the schema's is drift.
        let err = with(
            "autoland.rounds",
            Reading::new("autoland.rounds", json!(4), Origin::Default),
        )
        .unwrap_err();
        assert!(err.contains("the settings schema's is 5"), "{err}");
        // So is a derived default that isn't what its formula gives: with
        // maxJobs 4 and pipelineSlots 1, maxJobsPerProject's is 2.
        let err = with(
            "proj.maxJobsPerProject",
            Reading::new("proj.maxJobsPerProject", json!(1), Origin::Default),
        )
        .unwrap_err();
        assert!(err.contains("is 1, the settings schema's is 2"), "{err}");
        // A flake option can't come from a live file, nor a live one from the flake.
        assert!(
            with(
                "autoland.rounds",
                Reading::new("autoland.rounds", json!(3), Origin::Live)
            )
            .is_err()
        );
        assert!(
            with(
                "proj.roles",
                Reading::new("proj.roles", json!({}), Origin::Flake)
            )
            .is_err()
        );
        // A live file that isn't there leaves the default, and says which file.
        let r = Reading::live("proj.usageHold", "/jobs/.usage.json".into(), None);
        assert_eq!((r.origin, &r.value), (Origin::Default, &Value::Null));
        let doc = with("proj.usageHold", r).unwrap();
        assert!(doc.json().contains("/jobs/.usage.json"));
    }

    #[test]
    fn the_flakes_values_say_which_it_set() {
        let tracked = json!({
            "autoland.rounds": {"value": 3, "set": true, "default": 5},
            "autoland.jobIdleMinutes": {"value": 20, "set": false, "default": 20},
            "autoland.reviewers.codex.effort": null,
            "proj.maxJobs": {"value": 6, "set": true, "default": 4},
            "proj.pipelineSlots": {"value": 1, "set": false, "default": 1},
            "proj.maxJobsPerProject": {"value": 2, "set": true, "default": 4},
        });
        let got = flake(tracked.as_object().unwrap()).unwrap();
        let got: Vec<_> = got.iter().map(|r| (r.name.as_str(), r.origin)).collect();
        assert_eq!(
            got,
            vec![
                ("autoland.jobIdleMinutes", Origin::Default),
                ("autoland.reviewers.codex.effort", Origin::Default),
                ("autoland.rounds", Origin::Flake),
                ("proj.maxJobs", Origin::Flake),
                ("proj.maxJobsPerProject", Origin::Flake),
                ("proj.pipelineSlots", Origin::Default),
            ]
        );
        let changed = |name: &str, entry: Value| {
            let mut t = tracked.as_object().unwrap().clone();
            t.insert(name.into(), entry);
            flake(&t).unwrap_err()
        };
        // A literal default the flake overrides is still the schema's.
        let err = changed(
            "autoland.rounds",
            json!({"value": 3, "set": true, "default": 4}),
        );
        assert_eq!(
            err,
            "autoland.rounds: its module's default is 4, the settings schema's is 5"
        );
        // A derived default is what its formula gives from the flake's values,
        // overridden (maxJobsPerProject) or not (pipelineSlots).
        let err = changed(
            "proj.maxJobsPerProject",
            json!({"value": 2, "set": true, "default": 3}),
        );
        assert!(err.contains("is 3, the settings schema's is 4"), "{err}");
        let err = changed(
            "proj.pipelineSlots",
            json!({"value": 1, "set": true, "default": 0}),
        );
        assert_eq!(
            err,
            "proj.pipelineSlots: its module's default is 0, the settings schema's is 1"
        );
        // An entry without its module's default isn't one.
        let err = changed("autoland.rounds", json!({"value": 3, "set": true}));
        assert!(err.contains("expected"), "{err}");
        let err = flake(json!({"autoland.nope": null}).as_object().unwrap()).unwrap_err();
        assert!(err.contains("doesn't declare"), "{err}");
        let err = flake(json!({"proj.maxJobsPerProject": null}).as_object().unwrap()).unwrap_err();
        assert!(err.contains("derived"), "{err}");
    }
}
