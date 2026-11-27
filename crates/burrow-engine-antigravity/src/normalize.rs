//! Stream/JSON output normalization and Antigravity refusal classification.
//!
//! Normalizes `antigravity` headless output (`stream-json`, `json`, or `text`)
//! into `(session, result, structured_output, error, usage)`, validates
//! structured `--json-schema` output when requested, scrubs credentials from
//! error messages, and classifies quota/usage and login refusals inside this
//! adapter alone.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::OutputFormat;
use crate::schema::Schema;

/// Normalized token and cost usage extracted from an Antigravity turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_write_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub model: Option<String>,
}

impl Usage {
    fn is_empty(&self) -> bool {
        self.input_tokens.is_none()
            && self.output_tokens.is_none()
            && self.cache_read_tokens.is_none()
            && self.cache_write_tokens.is_none()
            && self.total_tokens.is_none()
            && self.cost_usd.is_none()
            && self.model.is_none()
    }

    fn merge(&mut self, other: Self) {
        if other.input_tokens.is_some() {
            self.input_tokens = other.input_tokens;
        }
        if other.output_tokens.is_some() {
            self.output_tokens = other.output_tokens;
        }
        if other.cache_read_tokens.is_some() {
            self.cache_read_tokens = other.cache_read_tokens;
        }
        if other.cache_write_tokens.is_some() {
            self.cache_write_tokens = other.cache_write_tokens;
        }
        if other.total_tokens.is_some() {
            self.total_tokens = other.total_tokens;
        }
        if other.cost_usd.is_some() {
            self.cost_usd = other.cost_usd;
        }
        if other.model.is_some() {
            self.model = other.model;
        }
    }
}

/// Engine-specific refusal classification owned solely by the Antigravity adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Refusal {
    /// Quota, rate-limit, or usage cap exhaustion.
    Quota {
        reset_at: Option<i64>,
        detail: String,
    },
    /// Missing, invalid, or expired authentication/login state.
    Login { detail: String },
    /// Continuation session state is missing or was replaced by a new conversation.
    SessionLost { detail: String },
}

/// Failure category for a headless Antigravity turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunError {
    Refusal(Refusal),
    ProcessFailed {
        exit_code: Option<i32>,
        signal: Option<i32>,
        message: String,
    },
    TimedOut,
    Cancelled,
    InvalidOutput(String),
    InvalidSchemaOutput(String),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refusal(Refusal::Quota {
                reset_at: Some(ts),
                detail,
            }) => write!(f, "antigravity quota refusal (resets at {ts}): {detail}"),
            Self::Refusal(Refusal::Quota {
                reset_at: None,
                detail,
            }) => write!(f, "antigravity quota refusal: {detail}"),
            Self::Refusal(Refusal::Login { detail }) => {
                write!(f, "antigravity login refusal: {detail}")
            }
            Self::Refusal(Refusal::SessionLost { detail }) => {
                write!(f, "antigravity session lost: {detail}")
            }
            Self::ProcessFailed {
                exit_code,
                signal,
                message,
            } => match (exit_code, signal) {
                (Some(code), _) => write!(f, "antigravity exited with status {code}: {message}"),
                (None, Some(sig)) => write!(f, "antigravity terminated by signal {sig}: {message}"),
                (None, None) => write!(f, "antigravity failed: {message}"),
            },
            Self::TimedOut => write!(f, "antigravity exceeded its time limit"),
            Self::Cancelled => write!(f, "antigravity invocation was cancelled"),
            Self::InvalidOutput(why) => write!(f, "antigravity returned unreadable output: {why}"),
            Self::InvalidSchemaOutput(why) => {
                write!(
                    f,
                    "antigravity structured output failed schema validation: {why}"
                )
            }
        }
    }
}

/// Normalized output of one Antigravity invocation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct TurnOutput {
    pub session: Option<String>,
    pub result: Option<String>,
    pub structured_output: Option<Value>,
    pub error: Option<RunError>,
    pub usage: Option<Usage>,
}

/// Normalize captured stdout/stderr and exit status into [`TurnOutput`].
pub fn normalize_output(
    format: OutputFormat,
    stdout: &[u8],
    stderr: &[u8],
    exit_code: Option<i32>,
    signal: Option<i32>,
    schema: Option<&Schema>,
    secrets: &[&str],
) -> TurnOutput {
    let stdout_str = String::from_utf8_lossy(stdout);
    let stderr_str = String::from_utf8_lossy(stderr);
    let succeeded = exit_code == Some(0) && signal.is_none();

    match format {
        OutputFormat::StreamJson => normalize_records(
            parse_jsonl(&stdout_str),
            &stdout_str,
            &stderr_str,
            succeeded,
            exit_code,
            signal,
            schema,
            secrets,
        ),
        OutputFormat::Json => normalize_records(
            parse_single_json(&stdout_str),
            &stdout_str,
            &stderr_str,
            succeeded,
            exit_code,
            signal,
            schema,
            secrets,
        ),
        OutputFormat::Text => normalize_text(
            &stdout_str,
            &stderr_str,
            succeeded,
            exit_code,
            signal,
            schema,
            secrets,
        ),
    }
}

enum ParsedRecords {
    Ok(Vec<Value>),
    Invalid(String),
}

fn parse_jsonl(stdout: &str) -> ParsedRecords {
    let mut records = Vec::new();
    for (idx, line) in stdout.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => records.push(v),
            Err(e) => {
                return ParsedRecords::Invalid(format!("line {} is not valid JSON: {e}", idx + 1));
            }
        }
    }
    ParsedRecords::Ok(records)
}

/// Unwrap Antigravity's `stream-json` envelope (`{"event": "<kind>", "<kind>": {...}}`).
fn unwrap_event(record: &Value) -> (Option<&str>, &Value) {
    let Some(event) = record.get("event").and_then(Value::as_str) else {
        return (None, record);
    };
    let payload = record
        .get(event)
        .filter(|v| v.is_object())
        .unwrap_or(record);
    (Some(event), payload)
}

fn record_has_error(v: &Value) -> bool {
    v.get("is_error").and_then(Value::as_bool).unwrap_or(false)
        || v.get("isError").and_then(Value::as_bool).unwrap_or(false)
        || v.get("status")
            .and_then(Value::as_str)
            .is_some_and(|s| s.eq_ignore_ascii_case("error") || s.eq_ignore_ascii_case("failed"))
        || v.get("error").is_some_and(|e| !e.is_null())
}

fn parse_single_json(stdout: &str) -> ParsedRecords {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return ParsedRecords::Ok(Vec::new());
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(v) => ParsedRecords::Ok(vec![v]),
        Err(e) => ParsedRecords::Invalid(format!("stdout is not valid JSON: {e}")),
    }
}

#[allow(clippy::too_many_arguments)]
fn normalize_records(
    parsed: ParsedRecords,
    stdout_str: &str,
    stderr_str: &str,
    succeeded: bool,
    exit_code: Option<i32>,
    signal: Option<i32>,
    schema: Option<&Schema>,
    secrets: &[&str],
) -> TurnOutput {
    let records = match parsed {
        ParsedRecords::Ok(records) => records,
        ParsedRecords::Invalid(why) => {
            let combined = combined_diagnostic(stdout_str, stderr_str);
            let error = if !succeeded {
                classify_refusal(&[], &combined, secrets)
                    .map(RunError::Refusal)
                    .unwrap_or_else(|| RunError::ProcessFailed {
                        exit_code,
                        signal,
                        message: scrub(&combined_or(&combined, &why), secrets),
                    })
            } else {
                RunError::InvalidOutput(scrub(&why, secrets))
            };
            return TurnOutput {
                error: Some(error),
                ..TurnOutput::default()
            };
        }
    };

    let mut session: Option<String> = None;
    let mut final_result: Option<String> = None;
    let mut streamed_chunks: Vec<String> = Vec::new();
    let mut structured_output: Option<Value> = None;
    let mut usage = Usage::default();
    let mut record_error_texts: Vec<String> = Vec::new();
    let mut saw_error_flag = false;
    let mut denied_actions: Vec<String> = Vec::new();
    let mut refusal_records: Vec<Value> = Vec::with_capacity(records.len() * 2);

    for record in &records {
        let (event_name, payload) = unwrap_event(record);
        refusal_records.push(record.clone());
        if !std::ptr::eq(payload, record) {
            refusal_records.push(payload.clone());
        }

        if session.is_none() {
            session = extract_session(record).or_else(|| extract_session(payload));
        }
        if let Some(u) = extract_usage(record) {
            usage.merge(u);
        }
        if !std::ptr::eq(payload, record)
            && let Some(u) = extract_usage(payload)
        {
            usage.merge(u);
        }
        if let Some(so) =
            extract_structured_output(payload).or_else(|| extract_structured_output(record))
        {
            structured_output = Some(so);
        }

        let record_type = event_name
            .or_else(|| record.get("type").and_then(Value::as_str))
            .unwrap_or_default();
        let is_err = record_has_error(record)
            || (!std::ptr::eq(payload, record) && record_has_error(payload))
            || record_type == "error";

        if is_err {
            saw_error_flag = true;
            if let Some(msg) = extract_error_text(payload).or_else(|| extract_error_text(record)) {
                record_error_texts.push(msg);
            }
            continue;
        }

        if let Some(arr) = payload.get("denied_actions").and_then(Value::as_array) {
            for item in arr {
                if let Some(name) = item
                    .get("action")
                    .or_else(|| item.get("display_name"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    denied_actions.push(name.to_owned());
                }
            }
        }

        if record_type == "result"
            || record.get("result").is_some()
            || record.get("output").is_some()
        {
            if let Some(text) = extract_result_text(payload).or_else(|| extract_result_text(record))
            {
                final_result = Some(text);
            }
        } else if record_type == "step_update" {
            if payload.get("step_type").and_then(Value::as_str) == Some("agent_response")
                && let Some(delta) = payload.get("text_delta").and_then(Value::as_str)
                && !delta.is_empty()
            {
                streamed_chunks.push(delta.to_owned());
            }
        } else if matches!(
            record_type,
            "assistant" | "message" | "content" | "delta" | "text"
        ) && let Some(text) = extract_assistant_text(payload)
        {
            streamed_chunks.push(text);
        }
    }

    let usage_opt = (!usage.is_empty()).then_some(usage);

    if saw_error_flag || !succeeded {
        let joined = join_nonempty(&record_error_texts, stderr_str);
        let fallback = if joined.is_empty() {
            stdout_str.trim().to_owned()
        } else {
            joined
        };
        let error = classify_refusal(&refusal_records, &fallback, secrets)
            .map(RunError::Refusal)
            .unwrap_or_else(|| RunError::ProcessFailed {
                exit_code,
                signal,
                message: scrub(
                    &combined_or(&fallback, "antigravity reported an error without details"),
                    secrets,
                ),
            });
        return TurnOutput {
            session,
            result: None,
            structured_output: None,
            error: Some(error),
            usage: usage_opt,
        };
    }

    let mut result =
        final_result.or_else(|| (!streamed_chunks.is_empty()).then(|| streamed_chunks.join("")));

    if structured_output.is_none()
        && result.as_deref().is_none_or(|s| s.trim().is_empty())
        && !denied_actions.is_empty()
    {
        let detail = scrub(
            &combined_or(
                stderr_str,
                &format!("denied actions: {}", denied_actions.join(", ")),
            ),
            secrets,
        );
        return TurnOutput {
            session,
            result: None,
            structured_output: None,
            error: Some(RunError::InvalidOutput(format!(
                "antigravity exited 0 without producing a result (denied actions: {}; {detail})",
                denied_actions.join(", ")
            ))),
            usage: usage_opt,
        };
    }

    if let Some(schema) = schema {
        match finalize_structured(schema, structured_output.take(), result.as_deref()) {
            Ok((value, rendered)) => {
                structured_output = Some(value);
                result = Some(rendered);
            }
            Err(why) => {
                return TurnOutput {
                    session,
                    result,
                    structured_output: None,
                    error: Some(RunError::InvalidSchemaOutput(scrub(&why, secrets))),
                    usage: usage_opt,
                };
            }
        }
    } else if result.is_none()
        && let Some(value) = &structured_output
        && let Ok(rendered) = serde_json::to_string(value)
    {
        result = Some(rendered);
    }

    if result.as_deref().is_none_or(|s| s.trim().is_empty()) {
        return TurnOutput {
            session,
            result: None,
            structured_output,
            error: Some(RunError::InvalidOutput(
                "antigravity exited 0 without producing a result".to_owned(),
            )),
            usage: usage_opt,
        };
    }

    TurnOutput {
        session,
        result,
        structured_output,
        error: None,
        usage: usage_opt,
    }
}

fn normalize_text(
    stdout_str: &str,
    stderr_str: &str,
    succeeded: bool,
    exit_code: Option<i32>,
    signal: Option<i32>,
    schema: Option<&Schema>,
    secrets: &[&str],
) -> TurnOutput {
    if !succeeded {
        let combined = combined_diagnostic(stdout_str, stderr_str);
        let error = classify_refusal(&[], &combined, secrets)
            .map(RunError::Refusal)
            .unwrap_or_else(|| RunError::ProcessFailed {
                exit_code,
                signal,
                message: scrub(
                    &combined_or(&combined, "antigravity exited unsuccessfully"),
                    secrets,
                ),
            });
        return TurnOutput {
            error: Some(error),
            ..TurnOutput::default()
        };
    }

    let trimmed = stdout_str.trim();
    if trimmed.is_empty() {
        return TurnOutput {
            error: Some(RunError::InvalidOutput(
                "antigravity produced empty text output".to_owned(),
            )),
            ..TurnOutput::default()
        };
    }

    if let Some(schema) = schema {
        match finalize_structured(schema, None, Some(trimmed)) {
            Ok((value, rendered)) => TurnOutput {
                result: Some(rendered),
                structured_output: Some(value),
                ..TurnOutput::default()
            },
            Err(why) => TurnOutput {
                result: Some(trimmed.to_owned()),
                error: Some(RunError::InvalidSchemaOutput(scrub(&why, secrets))),
                ..TurnOutput::default()
            },
        }
    } else {
        TurnOutput {
            result: Some(trimmed.to_owned()),
            ..TurnOutput::default()
        }
    }
}

/// The structured value a schema-bound invocation produced, held to `schema`:
/// the explicit `structured_output` when there is one, else the result text
/// parsed as JSON.
fn finalize_structured(
    schema: &Schema,
    explicit: Option<Value>,
    result_text: Option<&str>,
) -> Result<(Value, String), String> {
    let value = match explicit {
        Some(value) => value,
        None => {
            let Some(text) = result_text.map(str::trim).filter(|s| !s.is_empty()) else {
                return Err("missing structured JSON output".to_owned());
            };
            serde_json::from_str(text).map_err(|e| format!("output is not valid JSON: {e}"))?
        }
    };
    schema.validate(&value)?;
    let rendered = serde_json::to_string(&value).map_err(|e| e.to_string())?;
    Ok((value, rendered))
}

fn extract_session(v: &Value) -> Option<String> {
    for key in [
        "conversation_id",
        "conversationId",
        "session_id",
        "sessionId",
    ] {
        if let Some(s) = v.get(key).and_then(Value::as_str).map(str::trim)
            && !s.is_empty()
        {
            return Some(s.to_owned());
        }
    }
    for container in ["conversation", "session", "metadata"] {
        if let Some(obj) = v.get(container)
            && let Some(id) = obj
                .get("id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            return Some(id.to_owned());
        }
    }
    None
}

fn extract_structured_output(v: &Value) -> Option<Value> {
    for key in ["structured_output", "structuredOutput"] {
        if let Some(val) = v.get(key)
            && !val.is_null()
        {
            return Some(val.clone());
        }
    }
    None
}

fn extract_result_text(v: &Value) -> Option<String> {
    for key in ["result", "output", "response", "text"] {
        if let Some(s) = v.get(key).and_then(Value::as_str)
            && !s.trim().is_empty()
        {
            return Some(s.to_owned());
        }
    }
    extract_assistant_text(v)
}

fn extract_assistant_text(v: &Value) -> Option<String> {
    if let Some(s) = v.get("text").and_then(Value::as_str)
        && !s.is_empty()
    {
        return Some(s.to_owned());
    }
    if let Some(s) = v.get("delta").and_then(Value::as_str)
        && !s.is_empty()
    {
        return Some(s.to_owned());
    }
    if let Some(content) = v.get("content")
        && let Some(text) = extract_content_blocks(content)
    {
        return Some(text);
    }
    if let Some(content) = v.get("message").and_then(|m| m.get("content"))
        && let Some(text) = extract_content_blocks(content)
    {
        return Some(text);
    }
    None
}

fn extract_content_blocks(content: &Value) -> Option<String> {
    if let Some(s) = content.as_str() {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    let arr = content.as_array()?;
    let mut parts = Vec::new();
    for item in arr {
        if let Some(text) = item.get("text").and_then(Value::as_str)
            && !text.is_empty()
        {
            parts.push(text);
        }
    }
    (!parts.is_empty()).then(|| parts.join(""))
}

fn extract_usage(v: &Value) -> Option<Usage> {
    let mut usage = Usage::default();
    if let Some(model) = v
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        usage.model = Some(model.to_owned());
    }
    if let Some(cost) = v
        .get("cost_usd")
        .or_else(|| v.get("total_cost_usd"))
        .or_else(|| v.get("costUsd"))
        .and_then(Value::as_f64)
    {
        usage.cost_usd = Some(cost);
    }
    for key in ["usage", "usageMetadata", "usage_metadata"] {
        if let Some(obj) = v.get(key) {
            usage.merge(parse_usage_object(obj));
        }
    }
    if let Some(map) = v
        .get("modelUsage")
        .or_else(|| v.get("model_usage"))
        .and_then(Value::as_object)
        && let Some((model, obj)) = map.iter().next()
    {
        if usage.model.is_none() && !model.trim().is_empty() {
            usage.model = Some(model.clone());
        }
        usage.merge(parse_usage_object(obj));
    }
    (!usage.is_empty()).then_some(usage)
}

fn parse_usage_object(obj: &Value) -> Usage {
    let u64_of = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| obj.get(*k).and_then(Value::as_u64))
    };
    let input_tokens = u64_of(&[
        "input_tokens",
        "inputTokens",
        "prompt_tokens",
        "promptTokenCount",
    ]);
    let output_tokens = u64_of(&[
        "output_tokens",
        "outputTokens",
        "completion_tokens",
        "candidatesTokenCount",
    ]);
    let cache_read_tokens = u64_of(&[
        "cache_read_input_tokens",
        "cache_read_tokens",
        "cacheReadInputTokens",
        "cachedContentTokenCount",
    ]);
    let cache_write_tokens = u64_of(&[
        "cache_creation_input_tokens",
        "cache_write_tokens",
        "cacheCreationInputTokens",
    ]);
    let total_tokens =
        u64_of(&["total_tokens", "totalTokens", "totalTokenCount"]).or_else(|| {
            match (input_tokens, output_tokens) {
                (Some(i), Some(o)) => Some(i.saturating_add(o)),
                _ => None,
            }
        });
    let cost_usd = obj
        .get("cost_usd")
        .or_else(|| obj.get("costUSD"))
        .or_else(|| obj.get("costUsd"))
        .and_then(Value::as_f64);
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
        total_tokens,
        cost_usd,
        model: None,
    }
}

fn extract_error_text(v: &Value) -> Option<String> {
    if let Some(msg) = v
        .get("error")
        .and_then(|err| err.get("message"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(msg.to_owned());
    }
    for key in ["message", "result", "detail", "reason"] {
        if let Some(s) = v.get(key).and_then(Value::as_str).map(str::trim)
            && !s.is_empty()
        {
            return Some(s.to_owned());
        }
    }
    if let Some(s) = v
        .get("error")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(s.to_owned());
    }
    None
}

/// Classify Antigravity quota/usage or login/authentication refusals from
/// structured JSON error fields and diagnostic text.
pub fn classify_refusal(records: &[Value], text: &str, secrets: &[&str]) -> Option<Refusal> {
    let mut reset_at: Option<i64> = None;
    let mut is_quota = false;
    let mut is_login = false;

    for record in records {
        if let Some(ts) = extract_reset_timestamp(record) {
            reset_at = Some(reset_at.map_or(ts, |prev| prev.max(ts)));
        }
        for code in extract_error_codes(record) {
            let lower = code.to_ascii_lowercase();
            if is_quota_code(&lower) {
                is_quota = true;
            }
            if is_login_code(&lower) {
                is_login = true;
            }
        }
    }

    let lower_text = text.to_ascii_lowercase();
    if !is_quota && looks_like_quota(&lower_text) {
        is_quota = true;
    }
    if !is_login && looks_like_login(&lower_text) {
        is_login = true;
    }

    if is_login {
        let detail = scrub(
            &combined_or(text, "Antigravity authentication expired or missing"),
            secrets,
        );
        return Some(Refusal::Login { detail });
    }
    if is_quota {
        let detail = scrub(
            &combined_or(text, "Antigravity quota or usage limit reached"),
            secrets,
        );
        return Some(Refusal::Quota { reset_at, detail });
    }
    None
}

fn extract_reset_timestamp(v: &Value) -> Option<i64> {
    for container in [
        Some(v),
        v.get("error"),
        v.get("rate_limit"),
        v.get("rate_limit_info"),
        v.get("quota"),
    ]
    .into_iter()
    .flatten()
    {
        for key in ["resets_at", "resetsAt", "reset_at", "resetAt"] {
            if let Some(ts) = container.get(key).and_then(Value::as_i64)
                && ts > 0
            {
                return Some(ts);
            }
        }
    }
    None
}

fn extract_error_codes(v: &Value) -> Vec<String> {
    let mut codes = Vec::new();
    for container in [Some(v), v.get("error")].into_iter().flatten() {
        for key in ["code", "status", "error_type", "errorType", "reason"] {
            if let Some(val) = container.get(key) {
                if let Some(s) = val.as_str() {
                    codes.push(s.to_owned());
                } else if let Some(n) = val.as_i64() {
                    codes.push(n.to_string());
                }
            }
        }
    }
    if let Some(s) = v.get("error").and_then(Value::as_str) {
        codes.push(s.to_owned());
    }
    codes
}

fn is_quota_code(code: &str) -> bool {
    matches!(
        code,
        "resource_exhausted"
            | "quota_exceeded"
            | "rate_limit_exceeded"
            | "usage_limit_reached"
            | "too_many_requests"
            | "429"
    )
}

fn is_login_code(code: &str) -> bool {
    matches!(
        code,
        "unauthenticated"
            | "authentication_failed"
            | "auth_expired"
            | "login_required"
            | "token_expired"
            | "invalid_credentials"
            | "invalid_api_key"
            | "unauthorized"
            | "401"
    )
}

fn looks_like_quota(lower: &str) -> bool {
    lower.contains("resource_exhausted")
        || lower.contains("quota exceeded")
        || lower.contains("quota exhausted")
        || lower.contains("usage limit")
        || lower.contains("rate limit")
        || lower.contains("limit reached")
        || lower.contains("too many requests")
        || lower.contains("http 429")
        || lower.contains("status 429")
}

fn looks_like_login(lower: &str) -> bool {
    lower.contains("unauthenticated")
        || lower.contains("authentication failed")
        || lower.contains("authentication required")
        || lower.contains("failed to authenticate")
        || lower.contains("not logged in")
        || lower.contains("login required")
        || lower.contains("please log in")
        || lower.contains("please visit the url to log in")
        || lower.contains("sign in required")
        || lower.contains("oauth token expired")
        || lower.contains("session expired")
        || lower.contains("credentials expired")
        || lower.contains("invalid credentials")
        || lower.contains("invalid api key")
        || lower.contains("http 401")
        || lower.contains("status 401")
}

fn combined_diagnostic(stdout: &str, stderr: &str) -> String {
    join_nonempty(&[stdout.trim().to_owned()], stderr)
}

fn join_nonempty(parts: &[String], stderr: &str) -> String {
    let mut out: Vec<&str> = parts
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let stderr = stderr.trim();
    if !stderr.is_empty() && !out.contains(&stderr) {
        out.push(stderr);
    }
    out.join(": ")
}

fn combined_or(text: &str, fallback: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Remove any loaded secret material from diagnostic strings before emitting them.
pub fn scrub(text: &str, secrets: &[&str]) -> String {
    let mut clean = text.to_owned();
    for secret in secrets {
        let trimmed = secret.trim();
        if trimmed.len() >= 8 {
            clean = clean.replace(trimmed, "[redacted]");
        }
    }
    clean
}
