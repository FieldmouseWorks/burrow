//! The agents' own keys, should one turn up in what they say (an error
//! quoting its request, say): taken out of everything an agent says before
//! it's logged or kept.

use std::path::Path;

use serde_json::Value;

/// Nothing shorter is a key, an empty value least of all.
const SHORTEST: usize = 16;

/// Every string in Codex's login (`HOME/.codex/auth.json`) long enough to
/// be a key; none if there's no login to read.
pub fn secrets(home: &Path) -> Vec<String> {
    fn walk(x: &Value, found: &mut Vec<String>) {
        match x {
            Value::String(s) if s.chars().count() >= SHORTEST => found.push(s.clone()),
            Value::Object(m) => m.values().for_each(|v| walk(v, found)),
            Value::Array(a) => a.iter().for_each(|v| walk(v, found)),
            _ => {}
        }
    }
    let mut found = Vec::new();
    if let Some(login) = std::fs::read(home.join(".codex/auth.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    {
        walk(&login, &mut found);
    }
    found
}

/// x with those keys taken out, wherever they are in it.
pub fn scrub(x: &Value, keys: &[String]) -> Value {
    let mut text = x.to_string();
    for k in keys {
        let quoted = Value::String(k.clone()).to_string();
        text = text.replace(&quoted[1..quoted.len() - 1], "[redacted]");
    }
    serde_json::from_str(&text).unwrap_or_else(|_| Value::String("[redacted]".into()))
}

/// What an agent said, its keys taken out.
pub fn said(text: &str, keys: &[String]) -> String {
    match scrub(&Value::String(text.to_owned()), keys) {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

/// The last n characters of what an agent said, its keys taken out first so
/// none is cut in half.
pub fn tail(text: &str, n: usize, keys: &[String]) -> String {
    let text = said(text, keys);
    let skip = text.chars().count().saturating_sub(n);
    text.chars().skip(skip).collect()
}

/// The first n characters, the same way.
pub fn head(text: &str, n: usize, keys: &[String]) -> String {
    said(text, keys).chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{head, said, scrub, tail};

    /// A key goes wherever it is, escaped as JSON escapes it, and a cut
    /// never leaves half of one.
    #[test]
    fn keys_come_out_whole() {
        let keys = vec!["codexREFRESHtoken\"QWERTY\\zxcvb".to_owned()];
        let k = &keys[0];
        assert_eq!(
            scrub(&json!({"a": [format!("x{k}y")], "b": {"c": k}}), &keys),
            json!({"a": ["x[redacted]y"], "b": {"c": "[redacted]"}})
        );
        assert_eq!(said(&format!("panic: {k}"), &keys), "panic: [redacted]");
        assert_eq!(tail(&format!("{k} end"), 12, &keys), "edacted] end");
        assert_eq!(head(&format!("x {k}"), 5, &keys), "x [re");
        assert_eq!(said("nothing", &[]), "nothing");
    }
}
