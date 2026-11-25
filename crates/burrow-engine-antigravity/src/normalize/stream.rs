//! Real-time `stream-json` line normalization for `PROJ_STREAM_EVENTS=1`.
//!
//! Antigravity's headless `stream-json` mode wraps events in an envelope such as
//! `{"event":"result","result":{...}}` and uses Antigravity-specific field names
//! (`conversation_id`, `response`, `status`, `cache_read_tokens`, `cost_usd`).
//! `burrow-proj`'s transcript watcher (`Said` / `stream::result_of`) and
//! accounting (`burrow-probe`) expect top-level `"type":"result"` records with
//! canonical `is_error`, `result`, `session_id`, `usage`, and `total_cost_usd`
//! fields. [`StreamForwarder`] translates terminal result/error records as they
//! arrive while forwarding non-terminal lines unchanged.

use serde_json::{Map, Value, json};

use super::{
    Usage, extract_assistant_text, extract_error_text, extract_result_text, extract_session,
    extract_structured_output, extract_usage, record_has_error, unwrap_event,
};

/// Result of processing one raw stdout line for `PROJ_STREAM_EVENTS=1`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamStep {
    /// Line to forward in `{"event":"stream","line":...}`.
    pub line: String,
    /// First non-empty session ID observed across the stream, emitted once.
    pub new_session: Option<String>,
}

/// Stateful per-invocation normalizer for streamed stdout lines.
#[derive(Clone, Debug, Default)]
pub struct StreamForwarder {
    session: Option<String>,
    streamed_chunks: Vec<String>,
    usage: Usage,
    saw_error: bool,
    last_error: Option<String>,
}

impl StreamForwarder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process one raw NDJSON line from the Antigravity CLI.
    ///
    /// Non-JSON lines, non-terminal events (`init`, `step_update`, assistant
    /// chunks), and empty non-error results are forwarded verbatim. Terminal
    /// result and error records are normalized into `burrow-proj`'s top-level
    /// `{"type":"result",...}` transcript shape.
    pub fn process_line(&mut self, line: &str) -> StreamStep {
        let trimmed = line.trim();
        let Ok(record) = serde_json::from_str::<Value>(trimmed) else {
            return StreamStep {
                line: line.to_owned(),
                new_session: None,
            };
        };

        let (event_name, payload) = unwrap_event(&record);
        let new_session = if self.session.is_none() {
            let sid = extract_session(&record).or_else(|| extract_session(payload));
            if let Some(ref s) = sid {
                self.session = Some(s.clone());
            }
            sid
        } else {
            None
        };

        if let Some(u) = extract_usage(&record) {
            self.usage.merge(u);
        }
        if !std::ptr::eq(payload, &record)
            && let Some(u) = extract_usage(payload)
        {
            self.usage.merge(u);
        }

        let record_type = event_name
            .or_else(|| record.get("type").and_then(Value::as_str))
            .unwrap_or_default();

        if record_type == "step_update" {
            if payload.get("step_type").and_then(Value::as_str) == Some("agent_response")
                && let Some(delta) = payload.get("text_delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                self.streamed_chunks.push(delta.to_owned());
            }
            return StreamStep {
                line: line.to_owned(),
                new_session,
            };
        }

        if matches!(
            record_type,
            "assistant" | "message" | "content" | "delta" | "text"
        ) {
            if let Some(text) = extract_assistant_text(payload) {
                self.streamed_chunks.push(text);
            }
            return StreamStep {
                line: line.to_owned(),
                new_session,
            };
        }

        let is_terminal = matches!(record_type, "result" | "error");
        if !is_terminal {
            return StreamStep {
                line: line.to_owned(),
                new_session,
            };
        }

        let is_err = self.saw_error
            || record_has_error(&record)
            || (!std::ptr::eq(payload, &record) && record_has_error(payload))
            || record_type == "error";

        let normalized = if is_err {
            self.saw_error = true;
            if let Some(msg) = extract_error_text(payload).or_else(|| extract_error_text(&record)) {
                self.last_error = Some(msg);
            }
            let text = self
                .last_error
                .clone()
                .unwrap_or_else(|| "antigravity reported an error without details".to_owned());
            Some(self.build_result_record(true, text, payload, &record))
        } else {
            let structured =
                extract_structured_output(payload).or_else(|| extract_structured_output(&record));
            let text = extract_result_text(payload)
                .or_else(|| extract_result_text(&record))
                .or_else(|| {
                    (!self.streamed_chunks.is_empty()).then(|| self.streamed_chunks.join(""))
                })
                .filter(|s| !s.trim().is_empty())
                .or_else(|| {
                    structured
                        .as_ref()
                        .and_then(|v| serde_json::to_string(v).ok())
                });
            text.map(|t| self.build_result_record(false, t, payload, &record))
        };

        let out = normalized
            .and_then(|v| serde_json::to_string(&v).ok())
            .unwrap_or_else(|| line.to_owned());

        StreamStep {
            line: out,
            new_session,
        }
    }

    fn build_result_record(
        &self,
        is_error: bool,
        result_text: String,
        payload: &Value,
        record: &Value,
    ) -> Value {
        let mut map = Map::new();
        map.insert("type".to_owned(), Value::from("result"));
        map.insert(
            "subtype".to_owned(),
            Value::from(if is_error { "error" } else { "success" }),
        );
        map.insert("is_error".to_owned(), Value::from(is_error));
        map.insert("result".to_owned(), Value::from(result_text));

        if let Some(sid) = &self.session {
            map.insert("session_id".to_owned(), Value::from(sid.as_str()));
        }
        if let Some(turns) = extract_num_turns(payload).or_else(|| extract_num_turns(record)) {
            map.insert("num_turns".to_owned(), turns);
        }
        if has_token_usage(&self.usage) {
            map.insert(
                "usage".to_owned(),
                json!({
                    "input_tokens": self.usage.input_tokens.unwrap_or(0),
                    "output_tokens": self.usage.output_tokens.unwrap_or(0),
                    "cache_read_input_tokens": self.usage.cache_read_tokens.unwrap_or(0),
                    "cache_creation_input_tokens": self.usage.cache_write_tokens.unwrap_or(0),
                }),
            );
        }
        if let Some(cost) = self.usage.cost_usd {
            map.insert("total_cost_usd".to_owned(), Value::from(cost));
        }

        Value::Object(map)
    }
}

fn has_token_usage(u: &Usage) -> bool {
    u.input_tokens.is_some()
        || u.output_tokens.is_some()
        || u.cache_read_tokens.is_some()
        || u.cache_write_tokens.is_some()
}

fn extract_num_turns(v: &Value) -> Option<Value> {
    for key in ["num_turns", "numTurns"] {
        if let Some(val) = v.get(key)
            && val.is_number()
        {
            return Some(val.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_antigravity_success_envelope_with_usage_and_session() {
        let mut fwd = StreamForwarder::new();
        let init = r#"{"event":"init","conversation_id":"conv-1","init":{"model":"model-a"}}"#;
        let step1 = fwd.process_line(init);
        assert_eq!(step1.line, init);
        assert_eq!(step1.new_session.as_deref(), Some("conv-1"));

        let tool_err = r#"{"event":"step_update","step_update":{"conversation_id":"conv-1","step_index":1,"state":"ERROR","step_type":"tool","tool_name":"run_command","tool_info":{"error":{"type":"TOOL_ERROR","message":"recoverable"}}}}"#;
        let step2 = fwd.process_line(tool_err);
        assert_eq!(step2.line, tool_err);
        assert_eq!(step2.new_session, None);

        let raw_result = r#"{"event":"result","result":{"conversation_id":"conv-1","status":"SUCCESS","response":"All done\n","duration_seconds":2.5,"num_turns":2,"usage":{"input_tokens":120,"output_tokens":15,"cache_read_tokens":40,"total_tokens":135,"cost_usd":0.04}}}"#;
        let step3 = fwd.process_line(raw_result);
        assert_eq!(step3.new_session, None);
        let v: Value = serde_json::from_str(&step3.line).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["subtype"], "success");
        assert_eq!(v["is_error"], false);
        assert_eq!(v["result"], "All done\n");
        assert_eq!(v["session_id"], "conv-1");
        assert_eq!(v["num_turns"], 2);
        assert_eq!(
            v["usage"],
            json!({
                "input_tokens": 120,
                "output_tokens": 15,
                "cache_read_input_tokens": 40,
                "cache_creation_input_tokens": 0,
            })
        );
        assert_eq!(v["total_cost_usd"], 0.04);
    }

    #[test]
    fn normalizes_error_result_and_error_event_envelopes() {
        let mut fwd = StreamForwarder::new();
        let err_result = r#"{"event":"result","result":{"conversation_id":"conv-err-1","status":"ERROR","response":"","error":"RESOURCE_EXHAUSTED: quota exceeded","finished_reason":"error"}}"#;
        let step = fwd.process_line(err_result);
        assert_eq!(step.new_session.as_deref(), Some("conv-err-1"));
        let v: Value = serde_json::from_str(&step.line).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["subtype"], "error");
        assert_eq!(v["is_error"], true);
        assert_eq!(v["result"], "RESOURCE_EXHAUSTED: quota exceeded");
        assert_eq!(v["session_id"], "conv-err-1");
        assert!(v.get("usage").is_none());

        let mut fwd2 = StreamForwarder::new();
        let err_event =
            r#"{"event":"error","error":{"code":"UNAUTHENTICATED","message":"login expired"}}"#;
        let step2 = fwd2.process_line(err_event);
        let v2: Value = serde_json::from_str(&step2.line).unwrap();
        assert_eq!(v2["type"], "result");
        assert_eq!(v2["subtype"], "error");
        assert_eq!(v2["is_error"], true);
        assert_eq!(v2["result"], "login expired");

        // A trailing success-shaped result after an earlier error must stay an error.
        let trailing = fwd2.process_line(
            r#"{"event":"result","result":{"status":"SUCCESS","response":"ignored"}}"#,
        );
        let v3: Value = serde_json::from_str(&trailing.line).unwrap();
        assert_eq!(v3["type"], "result");
        assert_eq!(v3["subtype"], "error");
        assert_eq!(v3["is_error"], true);
        assert_eq!(v3["result"], "login expired");
    }

    #[test]
    fn falls_back_to_streamed_chunks_and_structured_output() {
        let mut fwd = StreamForwarder::new();
        fwd.process_line(r#"{"event":"init","init":{"model":"model-a"}}"#);
        fwd.process_line(
            r#"{"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"Hello "}}"#,
        );
        fwd.process_line(
            r#"{"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"world"}}"#,
        );
        let step =
            fwd.process_line(r#"{"event":"result","result":{"status":"SUCCESS","response":""}}"#);
        let v: Value = serde_json::from_str(&step.line).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["is_error"], false);
        assert_eq!(v["result"], "Hello world");
        // Model-only metadata from `init` must not fabricate an all-zero usage object.
        assert!(v.get("usage").is_none());

        let mut fwd2 = StreamForwarder::new();
        let step2 = fwd2.process_line(
            r#"{"event":"result","result":{"conversation_id":"conv-s","status":"SUCCESS","response":"","structured_output":{"verdict":"pass"}}}"#,
        );
        let v2: Value = serde_json::from_str(&step2.line).unwrap();
        assert_eq!(v2["type"], "result");
        assert_eq!(v2["is_error"], false);
        assert_eq!(v2["result"], r#"{"verdict":"pass"}"#);
        assert_eq!(v2["session_id"], "conv-s");
    }

    #[test]
    fn leaves_empty_non_error_result_and_non_terminal_records_unrewritten() {
        let mut fwd = StreamForwarder::new();
        let denied = r#"{"event":"result","result":{"conversation_id":"conv-deny-1","status":"SUCCESS","response":"","denied_actions":[{"action":"command","display_name":"Bash"}]}}"#;
        let step = fwd.process_line(denied);
        assert_eq!(step.line, denied);
        assert_eq!(step.new_session.as_deref(), Some("conv-deny-1"));

        let user =
            r#"{"type":"user","content":[{"type":"tool_result","content":"result"}],"result":"x"}"#;
        assert_eq!(fwd.process_line(user).line, user);

        let plain = "not json";
        assert_eq!(fwd.process_line(plain).line, plain);
    }

    #[test]
    fn normalizes_flat_result_with_antigravity_field_names() {
        let mut fwd = StreamForwarder::new();
        let flat = r#"{"type":"result","conversation_id":"conv-flat","response":"Done","usage":{"promptTokenCount":10,"candidatesTokenCount":5,"cachedContentTokenCount":3},"cost_usd":0.01}"#;
        let step = fwd.process_line(flat);
        assert_eq!(step.new_session.as_deref(), Some("conv-flat"));
        let v: Value = serde_json::from_str(&step.line).unwrap();
        assert_eq!(v["type"], "result");
        assert_eq!(v["subtype"], "success");
        assert_eq!(v["is_error"], false);
        assert_eq!(v["result"], "Done");
        assert_eq!(v["session_id"], "conv-flat");
        assert_eq!(
            v["usage"],
            json!({
                "input_tokens": 10,
                "output_tokens": 5,
                "cache_read_input_tokens": 3,
                "cache_creation_input_tokens": 0,
            })
        );
        assert_eq!(v["total_cost_usd"], 0.01);
    }
}
