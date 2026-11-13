//! Every pipeline option `settings show` knows, declared once. An option
//! read anywhere but listed nowhere here can't be shown: [`crate::show`]
//! refuses a value whose name isn't one of these, and wants one for each.
//!
//! The names of the options the flake sets are their paths under `burrow.`
//! in the NixOS modules (modules/settings/tracked.nix renders them); the
//! ones that change live are named for the file that keeps them today.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

/// Each option's value in effect, by name.
pub type Values = BTreeMap<String, Value>;

/// What one option's value is.
#[derive(Debug)]
pub enum Kind {
    Bool,
    /// A whole number, at least `min`.
    Count {
        min: u64,
    },
    Text,
    /// A number from 0 to 100, fractions included.
    Percent,
    /// One of these words.
    OneOf(&'static [&'static str]),
    /// A GitHub repository, OWNER/NAME.
    Repo,
    /// The kind, or null for none.
    Maybe(&'static Kind),
    List(&'static Kind),
    /// Any keys, each with a value of the kind.
    Map(&'static Kind),
    /// These fields, every one of them, and no other.
    Record(&'static [Field]),
    /// A JSON object the schema doesn't look inside (a per-repo override's
    /// settings, which its own module checks).
    Object,
}

#[derive(Debug)]
pub struct Field {
    pub name: &'static str,
    pub kind: &'static Kind,
}

/// How a change to it takes effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// It's set in the machine's flake: a change is a PR to it and a deploy.
    Deploy,
    /// It's kept in a file on the machine, and its readers read it again at
    /// their next decision: a change takes effect then, no deploy. `file`
    /// says which, as the guidance would name it: the reference is the one
    /// place that does (burrow-guidance holds the rest to that).
    Live { file: &'static str },
}

impl Change {
    pub fn name(self) -> &'static str {
        match self {
            Change::Deploy => "deploy",
            Change::Live { .. } => "live",
        }
    }
}

/// What it is when nothing sets it.
#[derive(Debug)]
pub enum Fallback {
    /// This value, as JSON.
    Value(&'static str),
    /// Worked out from other options' values, as `says` says.
    Derived {
        says: &'static str,
        from: fn(&Values) -> Option<Value>,
    },
}

/// One option.
#[derive(Debug)]
pub struct Setting {
    pub name: &'static str,
    pub kind: &'static Kind,
    pub default: Fallback,
    pub help: &'static str,
    /// The part of the pipeline that reads it.
    pub owner: &'static str,
    pub change: Change,
}

impl Setting {
    /// Its default, when it's a value rather than derived.
    pub fn default_value(&self) -> Option<Value> {
        match self.default {
            Fallback::Value(text) => Some(
                serde_json::from_str(text)
                    .unwrap_or_else(|e| panic!("{}: default {text}: {e}", self.name)),
            ),
            Fallback::Derived { .. } => None,
        }
    }

    /// Its default given the other options' values; None when a derived
    /// one's inputs aren't there.
    pub fn default_given(&self, values: &Values) -> Option<Value> {
        match self.default {
            Fallback::Value(_) => self.default_value(),
            Fallback::Derived { from, .. } => from(values),
        }
    }

    /// Where a value of it may come from.
    pub fn origins(&self) -> &'static [crate::show::Origin] {
        use crate::show::Origin;
        match self.change {
            Change::Deploy => &[Origin::Default, Origin::Flake],
            Change::Live { .. } => &[Origin::Default, Origin::Live],
        }
    }
}

/// The one entry for `name`.
pub fn find(name: &str) -> Option<&'static Setting> {
    SETTINGS.iter().find(|s| s.name == name)
}

/// The one runtime settings overlay, under `burrow.stateDir`.
const OVERLAY: &str = "settings/overlay.json";
/// Role routing and tuning, beside it under the same lock (roles::store).
const ROLES_OVERLAY: &str = "settings/overlay.roles.json";
const REPOS: Kind = Kind::List(&Kind::Repo);
const MINUTES: Kind = Kind::Count { min: 1 };
const SLOTS: Kind = Kind::Count { min: 1 };
const ENGINE: Kind = Kind::OneOf(&["claude", "codex"]);
const DRAIN: Kind = Kind::Maybe(&ENGINE);
const EFFORT: Kind = Kind::OneOf(&["low", "medium", "high", "xhigh"]);
const MAYBE_TEXT: Kind = Kind::Maybe(&Kind::Text);
const OVERFLOW: Kind = Kind::Maybe(&Kind::Record(&[
    Field {
        name: "pct",
        kind: &Kind::Percent,
    },
    Field {
        name: "engine",
        kind: &ENGINE,
    },
]));
const ROUTE: Kind = Kind::Record(&[
    Field {
        name: "engine",
        kind: &ENGINE,
    },
    Field {
        name: "over",
        kind: &OVERFLOW,
    },
]);
const ROLES: Kind = Kind::Map(&ROUTE);
const TUNING: Kind = Kind::Record(&[
    Field {
        name: "model",
        kind: &MAYBE_TEXT,
    },
    Field {
        name: "effort",
        kind: &MAYBE_TEXT,
    },
]);
const MODELS: Kind = Kind::Map(&TUNING);
const AUTHORS: Kind = Kind::List(&Kind::Record(&[
    Field {
        name: "login",
        kind: &Kind::Text,
    },
    Field {
        name: "id",
        kind: &Kind::Count { min: 1 },
    },
]));
const WORDS: Kind = Kind::List(&Kind::Text);
const OFFERS: Kind = Kind::Map(&Kind::Record(&[
    Field {
        name: "models",
        kind: &WORDS,
    },
    Field {
        name: "efforts",
        kind: &WORDS,
    },
]));
const SETUP: Kind = Kind::Record(&[
    Field {
        name: "roles",
        kind: &ROLES,
    },
    Field {
        name: "models",
        kind: &MODELS,
    },
]);
const SETUPS: Kind = Kind::Record(&[
    Field {
        name: "active",
        kind: &MAYBE_TEXT,
    },
    Field {
        name: "saved",
        kind: &Kind::Map(&SETUP),
    },
]);
const STAMP: Kind = Kind::Count { min: 0 };
const HOLD: Kind = Kind::Maybe(&Kind::Record(&[
    Field {
        name: "until",
        kind: &STAMP,
    },
    Field {
        name: "since",
        kind: &STAMP,
    },
    Field {
        name: "owner",
        kind: &Kind::Bool,
    },
    Field {
        name: "reason",
        kind: &Kind::Text,
    },
]));

/// A flake option's entry.
const fn flake(
    name: &'static str,
    kind: &'static Kind,
    default: Fallback,
    owner: &'static str,
    help: &'static str,
) -> Setting {
    Setting {
        name,
        kind,
        default,
        help,
        owner,
        change: Change::Deploy,
    }
}

/// A live file's entry.
const fn live(
    name: &'static str,
    kind: &'static Kind,
    default: &'static str,
    file: &'static str,
    help: &'static str,
) -> Setting {
    Setting {
        name,
        kind,
        default: Fallback::Value(default),
        help,
        owner: "proj",
        change: Change::Live { file },
    }
}

use Fallback::Value as Is;

fn count(values: &Values, name: &str) -> Option<i64> {
    values.get(name)?.as_i64()
}

/// modules/proj.nix: `if maxJobs > 1 then 1 else 0`.
fn pipeline_slots(values: &Values) -> Option<Value> {
    Some(json!(i64::from(count(values, "proj.maxJobs")? > 1)))
}

/// modules/proj.nix: `max 1 (maxJobs - pipelineSlots - 1)`.
fn per_project(values: &Values) -> Option<Value> {
    let left = count(values, "proj.maxJobs")? - count(values, "proj.pipelineSlots")? - 1;
    Some(json!(left.max(1)))
}

/// Every option, in the order `settings show` lists them.
pub static SETTINGS: &[Setting] = &[
    // proj: its slots and what it holds back.
    flake(
        "proj.maxJobs",
        &SLOTS,
        Is("4"),
        "proj",
        "How many turns run Claude at once; the rest wait in line.",
    ),
    flake(
        "proj.pipelineSlots",
        &Kind::Count { min: 0 },
        Fallback::Derived {
            says: "1 when maxJobs is more than 1, else 0",
            from: pipeline_slots,
        },
        "proj",
        "Of maxJobs, how many only the pipeline's own repos' jobs may take.",
    ),
    flake(
        "proj.maxJobsPerProject",
        &SLOTS,
        Fallback::Derived {
            says: "max 1 (maxJobs - pipelineSlots - 1)",
            from: per_project,
        },
        "proj",
        "The most slots one project's jobs hold at once.",
    ),
    flake(
        "proj.codex.slots",
        &SLOTS,
        Is("2"),
        "proj",
        "How many Codex turns run at once, in a pool of their own.",
    ),
    flake(
        "proj.usageTrickle",
        &SLOTS,
        Is("2"),
        "proj",
        "How many Claude turns run at once while the line from a usage hold drains after its reset.",
    ),
    flake(
        "proj.usage.paid",
        &Kind::Bool,
        Is("false"),
        "proj",
        "Whether paid extra usage is on; otherwise a usage limit means waiting for its reset.",
    ),
    flake(
        "authors",
        &AUTHORS,
        Is("[]"),
        "proj",
        "The GitHub accounts, by login and immutable numeric ID, whose issues and replies the inbox, engines door, probe, undo door and settings door act on.",
    ),
    flake(
        "settings.allowed",
        &REPOS,
        Is("[]"),
        "proj",
        "Tracked repositories whose automated work may move; the live repository overlay overrides these defaults.",
    ),
    live(
        "settings.repositoryOverrides",
        &Kind::Map(&Kind::Bool),
        "{}",
        OVERLAY,
        "Runtime repository permissions overriding settings.allowed; false vacates every role immediately.",
    ),
    live(
        "settings.pipelineUntil",
        &Kind::Maybe(&Kind::Count { min: 0 }),
        "null",
        OVERLAY,
        "Until this timestamp only pipeline repositories may move; null means no scoped hold.",
    ),
    // proj-inbox: sorting issues and handing ready ones on.
    flake(
        "proj.inbox.triage.drain",
        &DRAIN,
        Is("null"),
        "proj-inbox",
        "The engine `ready` issues are handed to on its idle slots; null hands none on.",
    ),
    flake(
        "proj.inbox.triage.drainJobs",
        &SLOTS,
        Is("2"),
        "proj-inbox",
        "Handed-on issues with a job at once, across every repo.",
    ),
    flake(
        "proj.inbox.triage.atOnce",
        &SLOTS,
        Is("2"),
        "proj-inbox",
        "Sorting jobs at once, across every repo.",
    ),
    // autoland: review rounds, waits and limits.
    flake(
        "autoland.rounds",
        &Kind::Count { min: 1 },
        Is("5"),
        "autoland",
        "Review rounds with findings before the reviewers stop and a judge decides.",
    ),
    flake(
        "autoland.handoffAfterMinutes",
        &MINUTES,
        Is("10"),
        "autoland",
        "How long findings or red CI sit untouched before a Claude job takes them.",
    ),
    flake(
        "autoland.sessionHandoffAfterMinutes",
        &MINUTES,
        Is("30"),
        "autoland",
        "The same wait for a PR whose own session is linked in its body.",
    ),
    flake(
        "autoland.settleDelayMinutes",
        &MINUTES,
        Is("1"),
        "autoland",
        "How long a new head sits still before the reviewers are asked.",
    ),
    flake(
        "autoland.stallAfterMinutes",
        &MINUTES,
        Is("120"),
        "autoland",
        "How long a PR may wait on one thing before the stall watchdog takes it.",
    ),
    flake(
        "autoland.trainStallAfterMinutes",
        &MINUTES,
        Is("240"),
        "autoland",
        "The same for a PR in its repo's merge train.",
    ),
    flake(
        "autoland.holdStallAfterMinutes",
        &MINUTES,
        Is("480"),
        "autoland",
        "The same for a PR on hold.",
    ),
    flake(
        "autoland.renovateMergeAfterMinutes",
        &MINUTES,
        Is("60"),
        "autoland",
        "How long a settled, green Renovate PR with automerge may sit before the watchdog takes it.",
    ),
    flake(
        "autoland.jobTimeoutMinutes",
        &MINUTES,
        Is("60"),
        "autoland",
        "How long each turn of a steward's Claude job may run.",
    ),
    flake(
        "autoland.jobIdleMinutes",
        &MINUTES,
        Is("20"),
        "autoland",
        "How long such a turn may say nothing before it's stuck and stopped.",
    ),
    flake(
        "autoland.proofTimeoutMinutes",
        &MINUTES,
        Is("180"),
        "autoland",
        "How long a proof build or run the steward does itself may take.",
    ),
    flake(
        "autoland.baselinePerPass",
        &Kind::Count { min: 1 },
        Is("2"),
        "autoland",
        "The most baseline jobs one pass starts.",
    ),
    flake(
        "autoland.repos",
        &Kind::Map(&Kind::Object),
        Is("{}"),
        "autoland",
        "Per-repo overrides (OWNER/NAME) of the steward's rounds, waits and checks.",
    ),
    // The reviewers.
    flake(
        "autoland.reviewers.codex.enable",
        &Kind::Bool,
        Is("false"),
        "autoland-review",
        "Whether Codex reviews every head.",
    ),
    flake(
        "autoland.reviewers.codex.effort",
        &EFFORT,
        Is(r#""high""#),
        "autoland-review",
        "How hard Codex thinks on a review.",
    ),
    flake(
        "autoland.reviewers.codex.parallel",
        &Kind::Count { min: 1 },
        Is("2"),
        "autoland-review",
        "The most Codex reviews at once.",
    ),
    flake(
        "autoland.reviewers.timeoutMinutes",
        &MINUTES,
        Is("30"),
        "autoland-review",
        "How long one review may run before it's a failed try.",
    ),
    flake(
        "autoland.reviewers.skipPaths",
        &Kind::List(&Kind::Text),
        Is(
            r#"["(^|/)secrets?/", "(^|/)\\.sops\\.yaml$", "\\.sops\\.(ya?ml|json|env)$", "(^|/)\\.env(\\..*)?$", "\\.(pem|key|p12|pfx)$"]"#,
        ),
        "autoland-review",
        "Path regexes left out of what the reviewers read.",
    ),
    // What changes live, in proj's files.
    live(
        "proj.roles",
        &ROLES,
        r#"{"*": {"engine": "claude", "over": null}, "planner": {"engine": "codex", "over": null}}"#,
        ROLES_OVERLAY,
        "Which engine runs each role (ROLE, ROLE@OWNER/REPO, * for every role, *@OWNER/REPO), and the engine it moves to when its own is short (over PCT ENGINE). The closest key wins: a named role over *, a repo's over the bare one.",
    ),
    live(
        "proj.models",
        &MODELS,
        r#"{"* claude": {"model": null, "effort": null}, "* codex": {"model": null, "effort": null}}"#,
        ROLES_OVERLAY,
        "Each role's model and effort on each engine (ROLE[@OWNER/REPO] ENGINE), keyed as proj.roles is; null is the engine's own setting. Each must be one proj.engineOffers lists.",
    ),
    live(
        "proj.engineOffers",
        &OFFERS,
        r#"{"claude": {"models": [], "efforts": ["low", "medium", "high", "xhigh", "max"]}, "codex": {"models": [], "efforts": ["minimal", "low", "medium", "high", "xhigh"]}}"#,
        ROLES_OVERLAY,
        "The models and efforts each engine offers on the machine: the only ones proj.models may name. Model ids live here, never in a tracked file.",
    ),
    live(
        "proj.engineSetups",
        &SETUPS,
        r#"{"active": null, "saved": {}}"#,
        ROLES_OVERLAY,
        "Saved engine setups (proj engines save/use), each a named set of proj.roles and proj.models values, and the one in force.",
    ),
    live(
        "proj.usageHold",
        &HOLD,
        "null",
        ".usage.json",
        "The usage hold in force: no Claude turn takes a slot until it ends. Null for none.",
    ),
];

impl Kind {
    /// Its JSON Schema.
    pub fn schema(&self) -> Value {
        match self {
            Kind::Bool => json!({"type": "boolean"}),
            Kind::Count { min } => json!({"type": "integer", "minimum": min}),
            Kind::Text => json!({"type": "string"}),
            Kind::Percent => json!({"type": "number", "minimum": 0, "maximum": 100}),
            Kind::OneOf(words) => json!({"enum": words}),
            Kind::Repo => json!({"type": "string", "pattern": "^[^/\\s]+/[^/\\s]+$"}),
            Kind::Maybe(kind) => json!({"anyOf": [{"type": "null"}, kind.schema()]}),
            Kind::List(kind) => json!({"type": "array", "items": kind.schema()}),
            Kind::Map(kind) => json!({"type": "object", "additionalProperties": kind.schema()}),
            Kind::Record(fields) => json!({
                "type": "object",
                "properties": fields.iter().map(|f| (f.name.to_owned(), f.kind.schema())).collect::<Map<_, _>>(),
                "required": fields.iter().map(|f| f.name).collect::<Vec<_>>(),
                "additionalProperties": false,
            }),
            Kind::Object => json!({"type": "object"}),
        }
    }

    /// Whether `value` is one of it, and where it isn't when not.
    pub fn check(&self, value: &Value) -> Result<(), String> {
        let wrong = || Err(format!("{value} is not {}", self.describe()));
        match (self, value) {
            (Kind::Bool, Value::Bool(_)) | (Kind::Text, Value::String(_)) => Ok(()),
            (Kind::Object, Value::Object(_)) => Ok(()),
            (Kind::Count { min }, Value::Number(n)) if n.as_u64().is_some_and(|n| n >= *min) => {
                Ok(())
            }
            (Kind::Percent, Value::Number(n))
                if n.as_f64().is_some_and(|n| (0.0..=100.0).contains(&n)) =>
            {
                Ok(())
            }
            (Kind::OneOf(words), Value::String(s)) if words.contains(&s.as_str()) => Ok(()),
            (Kind::Repo, Value::String(s)) if repo(s) => Ok(()),
            (Kind::Maybe(_), Value::Null) => Ok(()),
            (Kind::Maybe(kind), v) => kind.check(v),
            (Kind::List(kind), Value::Array(items)) => items
                .iter()
                .enumerate()
                .try_for_each(|(i, v)| kind.check(v).map_err(|e| format!("[{i}]: {e}"))),
            (Kind::Map(kind), Value::Object(items)) => items
                .iter()
                .try_for_each(|(k, v)| kind.check(v).map_err(|e| format!("{k}: {e}"))),
            (Kind::Record(fields), Value::Object(items)) => {
                if let Some(k) = items.keys().find(|k| !fields.iter().any(|f| f.name == *k)) {
                    return Err(format!("{k}: no such field"));
                }
                fields.iter().try_for_each(|f| match items.get(f.name) {
                    Some(v) => f.kind.check(v).map_err(|e| format!("{}: {e}", f.name)),
                    None => Err(format!("{}: missing", f.name)),
                })
            }
            _ => wrong(),
        }
    }

    fn describe(&self) -> String {
        match self {
            Kind::Bool => "true or false".into(),
            Kind::Count { min } => format!("a whole number from {min}"),
            Kind::Text => "text".into(),
            Kind::Percent => "a number from 0 to 100".into(),
            Kind::OneOf(words) => format!("one of {}", words.join(", ")),
            Kind::Repo => "OWNER/NAME".into(),
            Kind::Maybe(kind) => format!("null or {}", kind.describe()),
            Kind::List(kind) => format!("a list of {}", kind.describe()),
            Kind::Map(_) | Kind::Record(_) | Kind::Object => "an object".into(),
        }
    }
}

fn repo(s: &str) -> bool {
    matches!(s.split_once('/'), Some((owner, name))
        if !owner.is_empty() && !name.is_empty() && !name.contains('/')
            && !s.contains(char::is_whitespace))
}

/// The JSON Schema `settings show --json` is held to: every option once,
/// required, each with its value's type, its default, its help, its owner
/// (`x-owner`) and how it changes (`x-change`), and nothing else.
pub fn json_schema() -> Value {
    let options: Map<String, Value> = SETTINGS
        .iter()
        .map(|s| {
            let mut value = s.kind.schema();
            if let Some(default) = s.default_value() {
                value["default"] = default;
            }
            let help = match s.default {
                Fallback::Derived { says, .. } => format!("{} Default: {says}.", s.help),
                Fallback::Value(_) => s.help.to_owned(),
            };
            let origins: Vec<_> = s.origins().iter().map(|o| o.name()).collect();
            let entry = json!({
                "description": help,
                "x-owner": s.owner,
                "x-change": s.change.name(),
                "type": "object",
                "properties": {
                    "value": value,
                    "origin": {"enum": origins},
                    "file": {"type": "string"},
                },
                "required": ["value", "origin"],
                "additionalProperties": false,
            });
            (s.name.to_owned(), entry)
        })
        .collect();
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "burrow settings",
        "description": "Every pipeline option in effect on the machine, and where its value came from.",
        "type": "object",
        "properties": {
            "options": {
                "type": "object",
                "properties": options,
                "required": SETTINGS.iter().map(|s| s.name).collect::<Vec<_>>(),
                "additionalProperties": false,
            },
        },
        "required": ["options"],
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_name_once_and_each_default_its_own_kind() {
        let mut names: Vec<_> = SETTINGS.iter().map(|s| s.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), SETTINGS.len(), "a name declared twice");
        for s in SETTINGS {
            assert!(!s.help.is_empty() && !s.owner.is_empty(), "{}", s.name);
            if let Some(default) = s.default_value() {
                s.kind
                    .check(&default)
                    .unwrap_or_else(|e| panic!("{}: default: {e}", s.name));
            }
        }
    }

    #[test]
    fn kinds_take_their_values_and_refuse_others() {
        assert!(HOLD.check(&Value::Null).is_ok());
        let hold = json!({"until": 9, "since": 1, "owner": true, "reason": "r"});
        assert!(HOLD.check(&hold).is_ok());
        let err = HOLD
            .check(&json!({"until": 9, "since": 1, "owner": true}))
            .unwrap_err();
        assert_eq!(err, "reason: missing");
        assert!(HOLD.check(&json!({"until": -1})).is_err());
        assert!(REPOS.check(&json!(["sam/box"])).is_ok());
        assert!(REPOS.check(&json!(["sam"])).is_err());
        assert!(DRAIN.check(&json!("codex")).is_ok());
        assert!(DRAIN.check(&json!("gemini")).is_err());
        assert!(SLOTS.check(&json!(0)).is_err());
        let err = MODELS
            .check(&json!({"fix codex": {"model": "m", "effort": 3}}))
            .unwrap_err();
        assert!(err.starts_with("fix codex: effort: "), "{err}");
        assert!(
            ROLES
                .check(&json!({"fix": {"engine": "codex", "over": null}}))
                .is_ok()
        );
        let err = ROLES
            .check(&json!({"fix": {"engine": "gemini", "over": null}}))
            .unwrap_err();
        assert!(err.starts_with("fix: engine: "), "{err}");
    }

    #[test]
    fn the_json_schema_requires_every_option_and_no_other() {
        let schema = json_schema();
        let options = &schema["properties"]["options"];
        assert_eq!(options["additionalProperties"], false);
        let required: Vec<_> = options["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        let names: Vec<_> = SETTINGS.iter().map(|s| s.name).collect();
        assert_eq!(required, names);
        let rounds = &options["properties"]["autoland.rounds"];
        assert_eq!(rounds["x-change"], "deploy");
        assert_eq!(rounds["x-owner"], "autoland");
        assert_eq!(rounds["properties"]["value"]["default"], 5);
        let roles = &options["properties"]["proj.roles"];
        assert_eq!(roles["x-change"], "live");
        assert_eq!(
            roles["properties"]["origin"]["enum"],
            json!(["default", "live"])
        );
    }
}
