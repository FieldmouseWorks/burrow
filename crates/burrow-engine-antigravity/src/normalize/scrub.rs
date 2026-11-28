//! Credential scrubbing for Antigravity diagnostics, normalized outputs, and
//! real-time streamed stdout lines.

use serde_json::Value;

/// Remove any loaded secret material from emitted strings, matching both
/// verbatim text and its JSON-escaped interior (for secrets containing quotes,
/// backslashes, or newlines inside raw or truncated JSON fragments).
pub fn scrub(text: &str, secrets: &[&str]) -> String {
    let mut clean = text.to_owned();
    for secret in secrets {
        let trimmed = secret.trim();
        if trimmed.len() >= 8 {
            clean = clean.replace(trimmed, "[redacted]");
            if let Ok(quoted) = serde_json::to_string(trimmed)
                && let Some(escaped) = quoted.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
                && escaped != trimmed
            {
                clean = clean.replace(escaped, "[redacted]");
            }
        }
    }
    clean
}

/// Scrub every string value (and object key) inside a parsed JSON [`Value`],
/// returning `true` if anything was modified.
pub(super) fn scrub_value(value: &mut Value, secrets: &[&str]) -> bool {
    if secrets.is_empty() {
        return false;
    }
    match value {
        Value::String(s) => {
            let clean = scrub(s, secrets);
            if clean != *s {
                *s = clean;
                true
            } else {
                false
            }
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= scrub_value(item, secrets);
            }
            changed
        }
        Value::Object(map) => {
            let mut changed = false;
            let keys_dirty = map.keys().any(|k| scrub(k, secrets) != *k);
            if keys_dirty {
                let old = std::mem::take(map);
                for (k, mut v) in old {
                    let clean_k = scrub(&k, secrets);
                    changed |= clean_k != k;
                    changed |= scrub_value(&mut v, secrets);
                    map.insert(clean_k, v);
                }
            } else {
                for v in map.values_mut() {
                    changed |= scrub_value(v, secrets);
                }
            }
            changed
        }
        _ => false,
    }
}

/// Scrub any loaded secret material from one streamed stdout line while
/// preserving valid JSON framing (and exact bytes when nothing is redacted).
pub fn scrub_stream_line(line: &str, secrets: &[&str]) -> String {
    if secrets.is_empty() {
        return line.to_owned();
    }
    let trimmed = line.trim();
    if let Ok(mut value) = serde_json::from_str::<Value>(trimmed) {
        if scrub_value(&mut value, secrets) {
            let rendered = serde_json::to_string(&value).unwrap_or_else(|_| line.to_owned());
            return scrub(&rendered, secrets);
        }
        let raw = scrub(line, secrets);
        if raw != line {
            return raw;
        }
        return line.to_owned();
    }
    scrub(line, secrets)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_exact_json_line_when_no_secret_is_present() {
        let init = r#"{"event":"init","init":{"session_id":"ag-1","model":"model-a"}}"#;
        assert_eq!(scrub_stream_line(init, &["ya29.super-secret-token"]), init);
        assert_eq!(scrub_stream_line(init, &[]), init);
    }

    #[test]
    fn scrubs_json_escaped_credential_file_and_preserves_valid_json() {
        let raw_cred = r#"{"access_token":"ya29.secret-abc","extra":"unlisted-secret-xyz"}"#;
        let line = serde_json::json!({
            "event": "step_update",
            "step_update": {
                "tool_info": { "output": format!("dump:\n{raw_cred}\n") }
            }
        })
        .to_string();
        let clean = scrub_stream_line(&line, &[raw_cred, "ya29.secret-abc"]);
        assert!(!clean.contains("ya29.secret-abc"), "{clean}");
        assert!(!clean.contains("unlisted-secret-xyz"), "{clean}");
        let parsed: Value = serde_json::from_str(&clean).unwrap();
        assert_eq!(
            parsed["step_update"]["tool_info"]["output"],
            "dump:\n[redacted]\n"
        );
    }

    #[test]
    fn scrubs_raw_json_object_and_truncated_json_lines() {
        let raw_cred = r#"{"custom_key":"very-secret-value-123"}"#;
        // Line literally equals the credential JSON object.
        let clean_obj = scrub_stream_line(raw_cred, &[raw_cred]);
        assert_eq!(clean_obj, "[redacted]");

        // Truncated/invalid JSON line carrying the JSON-escaped credential string.
        let truncated =
            r#"{"event":"step_update","output":"{\"custom_key\":\"very-secret-value-123\"}"#;
        let clean_trunc = scrub_stream_line(truncated, &[raw_cred]);
        assert!(
            !clean_trunc.contains("very-secret-value-123"),
            "{clean_trunc}"
        );
        assert!(clean_trunc.contains("[redacted]"), "{clean_trunc}");
    }
}
