//! JSONL contains interleaved payloads, not one concatenated text. A text
//! field continues only the same path and producer/call/channel identity.
use super::{REDACTED, Redactor, SECRET_FIELD, independent_json, insert};
use serde_json::Value;
use std::collections::BTreeMap;

const IDENTITIES: &[&str] = &[
    "session_id",
    "thread_id",
    "job_id",
    "job",
    "call_id",
    "tool_use_id",
    "parent_tool_use_id",
    "item_id",
    "id",
    "role",
    "channel",
    "stream",
    "index",
];

#[derive(Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct Scope {
    path: Vec<Part>,
    identities: Vec<(Vec<Part>, String)>,
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Part {
    Field(String),
    Element(usize),
}

/// Context for a single output stream. Plain diagnostics are separate from
/// JSON payloads; stdout and stderr must each have their own instance.
#[derive(Default)]
pub struct Jsonl {
    payloads: BTreeMap<Scope, Redactor>,
    diagnostics: Redactor,
}
impl Jsonl {
    pub fn record(&mut self, value: &Value) -> Result<Value, String> {
        if value["type"] == "burrow.run" {
            self.payloads.clear();
            self.diagnostics = Redactor::default();
        }
        let mut scope = Scope::default();
        // Tool results may carry their call identity in a sibling envelope.
        for envelope in ["message", "item"] {
            if let Some(fields) = value.get(envelope).and_then(Value::as_object) {
                identify(fields, &[Part::Field(envelope.into())], &mut scope);
                if let Some(content) = fields.get("content").and_then(Value::as_array) {
                    for (index, entry) in content.iter().enumerate() {
                        if let Some(fields) = entry.as_object() {
                            identify(
                                fields,
                                &[
                                    Part::Field(envelope.into()),
                                    Part::Field("content".into()),
                                    Part::Element(index),
                                ],
                                &mut scope,
                            );
                        }
                    }
                }
            }
        }
        self.walk(value, scope)
    }

    fn walk(&mut self, value: &Value, mut scope: Scope) -> Result<Value, String> {
        Ok(match value {
            Value::String(text) => {
                Value::String(self.payloads.entry(scope).or_default().plain(text))
            }
            Value::Array(values) => Value::Array(
                values
                    .iter()
                    .enumerate()
                    .map(|(index, value)| {
                        let mut scope = scope.clone();
                        // String arrays are fragments of one field. Objects are
                        // independent entries, even without explicit producer IDs.
                        if !value.is_string() {
                            scope.path.push(Part::Element(index));
                        }
                        self.walk(value, scope)
                    })
                    .collect::<Result<_, _>>()?,
            ),
            Value::Object(fields) => {
                identify(fields, &scope.path.clone(), &mut scope);
                let mut clean = serde_json::Map::new();
                for (key, value) in fields {
                    let value = if SECRET_FIELD.is_match(key) {
                        Value::String(REDACTED.into())
                    } else if metadata(key) {
                        independent_json(value)?
                    } else {
                        let mut child = scope.clone();
                        child.path.push(Part::Field(key.clone()));
                        self.walk(value, child)?
                    };
                    insert(&mut clean, key, value)?;
                }
                Value::Object(clean)
            }
            value => value.clone(),
        })
    }

    /// One physical record or plain diagnostic line, with its line ending.
    pub fn line(&mut self, text: &str) -> Result<String, String> {
        let Ok(value) = serde_json::from_str(text) else {
            let mut clean = self.diagnostics.plain(text);
            // Hidden diagnostic text must not swallow the physical boundary
            // before the next framed JSON record.
            if text.ends_with("\r\n") && !clean.ends_with("\r\n") {
                clean.push_str("\r\n");
            } else if text.ends_with('\n') && !clean.ends_with('\n') {
                clean.push('\n');
            }
            return Ok(clean);
        };
        let mut clean = serde_json::to_string(&self.record(&value)?).map_err(|e| e.to_string())?;
        if text.ends_with("\r\n") {
            clean.push_str("\r\n");
        } else if text.ends_with('\n') {
            clean.push('\n');
        }
        Ok(clean)
    }

    pub fn text(&mut self, text: &str) -> Result<String, String> {
        if serde_json::from_str::<Value>(text).is_ok() {
            return self.line(text);
        }
        text.split_inclusive('\n')
            .map(|line| self.line(line))
            .collect()
    }
}

fn metadata(key: &str) -> bool {
    IDENTITIES.contains(&key)
        || matches!(
            key,
            "type" | "state" | "status" | "name" | "command" | "metadata"
        )
}

fn identify(fields: &serde_json::Map<String, Value>, path: &[Part], scope: &mut Scope) {
    for key in IDENTITIES {
        if let Some(value) = fields.get(*key).filter(|v| v.is_string() || v.is_number()) {
            let mut path = path.to_vec();
            path.push(Part::Field((*key).into()));
            let identity = (path, value.to_string());
            if !scope.identities.contains(&identity) {
                scope.identities.push(identity);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Jsonl;
    use serde_json::json;

    #[test]
    fn sibling_call_identity_routes_output_and_run_boundaries_reset_context() {
        let mut stream = Jsonl::default();
        let record = |call: &str, text: &str| json!({"session_id":"s", "message":{"role":"user", "content":[{"tool_use_id":call}]}, "tool_use_result":text});
        stream.record(&record("a", "password=\"first")).unwrap();
        let public = record("b", "cat \"public\"");
        assert_eq!(stream.record(&public).unwrap(), public);
        let end = stream.record(&record("a", "second\" suffix")).unwrap();
        assert!(!end.to_string().contains("second"));
        assert!(end["tool_use_result"].as_str().unwrap().contains("suffix"));
        stream.record(&record("a", "password=\"truncated")).unwrap();
        stream.line("password=\"diagnostic\n").unwrap();
        stream
            .record(&json!({"type":"burrow.run", "at":2}))
            .unwrap();
        let public = record("a", "new run public output");
        assert_eq!(stream.record(&public).unwrap(), public);
        assert_eq!(
            stream.line("new run diagnostic\n").unwrap(),
            "new run diagnostic\n"
        );
    }

    #[test]
    fn diagnostic_continuations_are_separate_from_json_metadata() {
        let mut stream = Jsonl::default();
        let text = "password=\"first\n{\"command\":\"cat \\\"$file\\\"\",\"output\":\"public\"}\nsecond\" diagnostic suffix";
        let clean = stream.text(text).unwrap();
        assert!(!clean.contains("first") && !clean.contains("second"));
        assert!(clean.contains("diagnostic suffix"));
        let record: serde_json::Value =
            serde_json::from_str(clean.lines().nth(1).unwrap()).unwrap();
        assert_eq!(record["command"], "cat \"$file\"");
        assert_eq!(record["output"], "public");
    }
}
