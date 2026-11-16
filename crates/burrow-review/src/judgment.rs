//! A judge's own answer contract; the verdict words belong to Conary.
use conary_core::plugin::Judgment;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answer {
    verdict: Judgment,
    why: String,
}

pub fn schema() -> Value {
    let words = [
        Judgment::Pass,
        Judgment::Fail,
        Judgment::Owner,
        Judgment::Misdescribed,
    ]
    .map(Judgment::word);
    json!({"type":"object", "properties": {
        "verdict":{"type":"string", "enum":words}, "why":{"type":"string"}
    }, "required":["verdict","why"], "additionalProperties":false})
}

pub fn validate(value: &Value) -> Result<(), String> {
    let answer: Answer =
        serde_json::from_value(value.clone()).map_err(|e| format!("judge answer: {e}"))?;
    let _ = answer.verdict;
    if answer.why.trim().is_empty() {
        return Err("judge gave no reasons".into());
    }
    Ok(())
}
