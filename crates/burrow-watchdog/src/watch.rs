//! The sources' watch files, and what's past its deadline in them.

use std::collections::BTreeMap;
use std::fs;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{Config, Source};
use crate::words::clock;

/// What a source waits on, as its watch file has it.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Item {
    pub what: String,
    /// What it waits on now, and since when: a new phase is progress.
    pub phase: String,
    pub since: f64,
    /// When it should have moved on by.
    pub due: f64,
    pub link: Option<String>,
    /// The burrow-events pass to kick for it: HANDLER[/OWNER/REPO].
    pub kick: Option<String>,
    /// A turn waiting for a slot in proj's line (proj's job_wait), which a job
    /// of its own would only join.
    #[serde(default)]
    pub line: bool,
}

/// An item past its deadline, or a source stuck itself (`key` "").
#[derive(Clone, Debug, PartialEq)]
pub struct Overdue {
    pub source: String,
    pub key: String,
    pub item: Item,
}

/// A source's watch file: when it was written, and its items. None when
/// there's none to read (not written yet, or half of one).
pub fn watched(src: &Source) -> Option<(f64, BTreeMap<String, Item>)> {
    let got: Value = serde_json::from_slice(&fs::read(&src.file).ok()?).ok()?;
    let at = got.get("at").and_then(Value::as_number)?.as_f64()?;
    let items = got.get("items")?.as_object()?;
    Some((
        at,
        items
            .iter()
            .filter(|(key, _)| !key.is_empty())
            .filter_map(|(key, it)| Some((key.clone(), item(it)?)))
            .collect(),
    ))
}

/// One item of a watch file; None when it's malformed, which leaves it out.
fn item(it: &Value) -> Option<Item> {
    let text = |k| it.get(k).and_then(Value::as_str).map(str::to_owned);
    let number = |k| {
        it.get(k)
            .and_then(Value::as_number)
            .and_then(|n| n.as_f64())
    };
    Some(Item {
        what: text("what")?,
        phase: text("phase")?,
        since: number("since")?,
        due: number("due")?,
        link: text("link"),
        kick: text("kick"),
        line: it.get("line").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// A stall: the item, in the phase it entered at `since`. A new phase is
/// another stall, from the first step.
pub fn sid(source: &str, key: &str, since: f64) -> String {
    format!("{source}\t{key}\t{since:.0}")
}

/// Every stall there is now, by sid, and when the next item falls due. A
/// source with no watch file to read is noted in `missing`, since when.
pub fn overdue(
    cfg: &Config,
    now: f64,
    missing: &mut BTreeMap<String, f64>,
) -> (BTreeMap<String, Overdue>, Option<f64>) {
    let mut found = BTreeMap::new();
    let mut due: Vec<f64> = Vec::new();
    for (name, src) in &cfg.sources {
        let got = watched(src);
        let at = match &got {
            Some((at, _)) => {
                missing.remove(name);
                *at
            }
            None => *missing.entry(name.clone()).or_insert(now),
        };
        if let Some(stale) = src.stale {
            if now >= at + stale {
                // Stuck itself: what it waits on is frozen with it.
                let phase = if got.is_some() {
                    format!("its watch file hasn't changed since {}", clock(at))
                } else {
                    format!("it has written no watch file since {}", clock(at))
                };
                found.insert(
                    sid(name, "", at),
                    Overdue {
                        source: name.clone(),
                        key: String::new(),
                        item: Item {
                            what: src.about.clone(),
                            phase,
                            since: at,
                            due: at + stale,
                            link: None,
                            kick: src.kick.clone(),
                            line: false,
                        },
                    },
                );
                continue;
            }
            due.push(at + stale);
        }
        for (key, it) in got.map(|(_, items)| items).unwrap_or_default() {
            if it.due <= now {
                found.insert(
                    sid(name, &key, it.since),
                    Overdue {
                        source: name.clone(),
                        key,
                        item: it,
                    },
                );
            } else {
                due.push(it.due);
            }
        }
    }
    (found, due.into_iter().reduce(f64::min))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{sid, watched};
    use crate::config::Source;

    // What a source writes that isn't an item is left out, and a file that
    // isn't a watch file is none.
    #[test]
    fn bad_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = Source {
            file: dir.path().join("w.json"),
            about: "x".into(),
            look: "y".into(),
            kick: None,
            stale: None,
        };
        let ok = r#"{"what": "x", "phase": "y", "since": 1, "due": 2, "link": 5, "kick": null}"#;
        std::fs::write(
            &src.file,
            format!(
                r#"{{"at": 9, "items": {{"a": "no", "b": {{"what": "x", "phase": "y", "since": "1", "due": 2}},
                    "": {ok}, "c": {ok}, "d": {{"what": "x", "phase": "y", "since": true, "due": 2}}}}}}"#
            ),
        )
        .unwrap();
        let (at, items) = watched(&src).unwrap();
        assert_eq!(at, 9.0);
        assert_eq!(items.keys().collect::<Vec<_>>(), ["c"]);
        assert_eq!(items["c"].link, None);
        for bad in [
            "{half",
            r#"{"at": true, "items": {}}"#,
            r#"{"at": 1, "items": []}"#,
            r#"{"items": {}}"#,
            "[]",
        ] {
            std::fs::write(&src.file, bad).unwrap();
            assert_eq!(watched(&src), None, "{bad}");
        }
        let none = Source {
            file: PathBuf::from("/nonexistent/w.json"),
            ..src
        };
        assert_eq!(watched(&none), None);
    }

    // A stall's name rounds its phase's start to the second, as the Python
    // watchdog's did: a stall it left carries on.
    #[test]
    fn sids() {
        assert_eq!(
            sid("autoland", "sam/r#7", 1799989200.4),
            "autoland\tsam/r#7\t1799989200"
        );
        assert_eq!(sid("autoland", "", 1799989200.6), "autoland\t\t1799989201");
    }
}
