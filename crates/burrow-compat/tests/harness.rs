//! The harness against real serde readers and writers, including a
//! non-JSON encoding. The overlay mirrors burrow-settings' deny_unknown_fields
//! document, whose repository admission field is what rollback must survive.
use burrow_compat::{AdapterError, Direction, Format, Registry, Stage, declared_version};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq)]
struct Overlay {
    repos: BTreeMap<String, bool>,
    admission: Option<String>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StrictV1 {
    #[serde(default)]
    repos: BTreeMap<String, bool>,
}

/// The v1 reader after the compatibility fix: it skips what it doesn't know.
#[derive(Default, Deserialize, Serialize)]
struct TolerantV1 {
    #[serde(default)]
    repos: BTreeMap<String, bool>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct V2 {
    #[serde(default)]
    repos: BTreeMap<String, bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admission: Option<String>,
}

fn sample(admission: Option<&str>) -> Overlay {
    Overlay {
        repos: BTreeMap::from([("a/b".into(), true)]),
        admission: admission.map(str::to_owned),
    }
}

fn v1_write(v: &Overlay) -> Result<Vec<u8>, AdapterError> {
    if let Some(a) = &v.admission {
        return Err(AdapterError::field(
            "admission",
            format!("v1 cannot hold {a}"),
        ));
    }
    Ok(serde_json::to_vec(&StrictV1 {
        repos: v.repos.clone(),
    })?)
}

fn v2_read(b: &[u8]) -> Result<Overlay, AdapterError> {
    let v: V2 = serde_json::from_slice(b)?;
    Ok(Overlay {
        repos: v.repos,
        admission: v.admission,
    })
}

fn v2_write(v: &Overlay) -> Result<Vec<u8>, AdapterError> {
    Ok(serde_json::to_vec(&V2 {
        repos: v.repos.clone(),
        admission: v.admission.clone(),
    })?)
}

fn overlay(strict: bool) -> Format<Overlay> {
    let v1_read = move |b: &[u8]| -> Result<Overlay, AdapterError> {
        let repos = if strict {
            serde_json::from_slice::<StrictV1>(b)?.repos
        } else {
            serde_json::from_slice::<TolerantV1>(b)?.repos
        };
        Ok(Overlay {
            repos,
            admission: None,
        })
    };
    Format::new("settings-overlay")
        .version(1, v1_read, v1_write, vec![sample(None)])
        .version(
            2,
            v2_read,
            v2_write,
            vec![sample(None), sample(Some("open"))],
        )
        .migrate(
            1,
            Ok,
            // Rolling back drops admission; that is the migration's choice, made explicit.
            |mut v| {
                v.admission = None;
                Ok(v)
            },
        )
}

#[test]
fn both_directions_pass_with_the_tolerant_reader() {
    overlay(false).assert_compatible();
}

#[test]
fn an_added_admission_field_fails_a_strict_old_reader_naming_it() {
    let failures = overlay(true).check();
    assert_eq!(failures.len(), 1, "{failures:?}");
    let f = &failures[0];
    assert_eq!(f.format, "settings-overlay");
    assert_eq!(f.direction, Direction::Backward);
    assert_eq!((f.writer, f.reader, f.stage), (2, 1, Stage::Read));
    assert_eq!(f.field.as_deref(), Some("admission"));
    let text = f.to_string();
    assert!(text.contains("settings-overlay") && text.contains("backward"));
    assert!(text.contains("admission"), "{text}");
}

#[test]
fn a_failing_migration_is_reported_with_its_field() {
    let f = Format::new("lossy")
        .version(
            1,
            |b| Ok(String::from_utf8_lossy(b).into_owned()),
            |v: &String| Ok(v.clone().into_bytes()),
            vec!["x".into()],
        )
        .version(
            2,
            |b| Ok(String::from_utf8_lossy(b).into_owned()),
            |v: &String| Ok(v.clone().into_bytes()),
            vec!["y".into()],
        )
        .migrate(1, Ok, |_| {
            Err(AdapterError::field("name", "cannot be represented in v1"))
        });
    let failures = f.check();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].stage, Stage::Migrate);
    assert_eq!(failures[0].direction, Direction::Backward);
    assert_eq!(failures[0].field.as_deref(), Some("name"));
}

#[test]
fn a_missing_migration_and_a_wrong_one_are_reported() {
    let base = |up: fn(String) -> Result<String, AdapterError>| {
        let f = Format::new("pair")
            .version(
                1,
                |b| Ok(String::from_utf8_lossy(b).into_owned()),
                |v: &String| Ok(v.clone().into_bytes()),
                vec!["x".into()],
            )
            .version(
                2,
                |b| Ok(String::from_utf8_lossy(b).into_owned()),
                |v: &String| Ok(v.clone().into_bytes()),
                vec![],
            );
        f.migrate(1, up, Ok)
    };
    // Bytes pass through unchanged, so a migration that rewrites the value disagrees with the reader.
    let failures = base(|v| Ok(v + "!")).check();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].stage, Stage::Compare);
    assert_eq!(failures[0].direction, Direction::Forward);
    // No migration at all.
    let none = Format::new("pair")
        .version(
            1,
            |b| Ok(String::from_utf8_lossy(b).into_owned()),
            |v: &String| Ok(v.clone().into_bytes()),
            vec!["x".into()],
        )
        .version(
            2,
            |b| Ok(String::from_utf8_lossy(b).into_owned()),
            |v: &String| Ok(v.clone().into_bytes()),
            vec![],
        );
    let failures = none.check();
    assert!(
        failures
            .iter()
            .any(|f| f.stage == Stage::Migrate && f.message.contains("no migration"))
    );
}

/// A non-JSON record: `key=value` lines.
fn lines_write(v: &BTreeMap<String, String>) -> Result<Vec<u8>, AdapterError> {
    Ok(v.iter()
        .map(|(k, v)| format!("{k}={v}\n"))
        .collect::<String>()
        .into_bytes())
}

fn lines_read(
    known: &'static [&'static str],
) -> impl Fn(&[u8]) -> Result<BTreeMap<String, String>, AdapterError> {
    move |b| {
        let mut out = BTreeMap::new();
        for line in String::from_utf8_lossy(b).lines() {
            let (k, v) = line
                .split_once('=')
                .ok_or_else(|| AdapterError::new(format!("no `=` in `{line}`")))?;
            if !known.contains(&k) {
                return Err(AdapterError::field(k, "unknown key"));
            }
            out.insert(k.to_owned(), v.to_owned());
        }
        Ok(out)
    }
}

#[test]
fn a_non_json_format_names_the_offending_key() {
    let s = |pairs: &[(&str, &str)]| {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect::<BTreeMap<_, _>>()
    };
    let f = Format::new("lines")
        .version(1, lines_read(&["a"]), lines_write, vec![s(&[("a", "1")])])
        .version(
            2,
            lines_read(&["a", "b"]),
            lines_write,
            vec![s(&[("a", "1"), ("b", "2")])],
        )
        .migrate(1, Ok, |mut v| {
            v.remove("b");
            Ok(v)
        });
    let failures = f.check();
    assert_eq!(failures.len(), 1, "{failures:?}");
    assert_eq!(failures[0].field.as_deref(), Some("b"));
    assert_eq!(failures[0].direction, Direction::Backward);
}

#[test]
fn unversioned_records_read_as_the_legacy_version() {
    assert_eq!(
        declared_version(br#"{"repos":{}}"#, "format_version").unwrap(),
        None
    );
    assert_eq!(
        declared_version(br#"{"format_version":2}"#, "format_version").unwrap(),
        Some(2)
    );
    let e = declared_version(br#"{"format_version":"x"}"#, "format_version").unwrap_err();
    assert_eq!(e.field.as_deref(), Some("format_version"));
    assert!(declared_version(b"[]", "format_version").is_err());
}

/// Stamping the version key before the legacy reader tolerates it is the
/// bug adoption must avoid; the harness fails it, naming the key.
#[test]
fn stamping_a_version_key_breaks_a_strict_legacy_reader() {
    let stamp = |stamped: bool| {
        let write = move |v: &BTreeMap<String, bool>| -> Result<Vec<u8>, AdapterError> {
            let mut o = serde_json::json!({ "repos": v });
            if stamped {
                o["format_version"] = 2.into();
            }
            Ok(serde_json::to_vec(&o)?)
        };
        let read = |b: &[u8]| -> Result<BTreeMap<String, bool>, AdapterError> {
            Ok(serde_json::from_slice::<StrictV1>(b)?.repos)
        };
        let sample = vec![BTreeMap::from([("a/b".to_owned(), true)])];
        Format::new("adopt")
            .version(1, read, write, sample.clone())
            .version(2, read, write, sample)
            .migrate(1, Ok, Ok)
    };
    stamp(false).assert_compatible();
    let failures = stamp(true).check();
    assert!(
        failures
            .iter()
            .all(|f| f.field.as_deref() == Some("format_version")),
        "{failures:?}"
    );
    assert!(!failures.is_empty());
}

#[test]
fn a_registry_refuses_a_second_format_with_the_same_id() {
    let mut r = Registry::new();
    r.register(overlay(false)).unwrap();
    let e = r.register(overlay(false)).unwrap_err();
    assert!(e.contains("settings-overlay"));
    assert!(r.check().is_empty());
    let mut bad = Registry::new();
    bad.register(overlay(true)).unwrap();
    assert_eq!(bad.check().len(), 1);
}
