//! Claude stream and usage contracts, including empty resume results.

use super::{claude_limit, is_error};
use serde_json::Value;

/// Whether a run ended refused for usage, and if so when Claude Code says
/// the limit resets (None when it doesn't) and its words. `last` is the
/// run's result; `stream` its lines, where the reset time is as a
/// timestamp; failing that, its words say it ([`crate::limit`]), read at
/// `now`.
pub fn refused(last: &Value, stream: &str, now: i64) -> Option<(Option<i64>, String)> {
    if !is_error(last) {
        return None;
    }
    let said = last["result"].as_str().unwrap_or_default().trim();
    if last["api_error"] != "usage_limit_reached" && !limit_words(said) {
        return None;
    }
    let mut until = None;
    for v in stream
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
    {
        for info in [
            &v["api_error_params"]["rate_limit_info"],
            &v["rate_limit_info"],
        ] {
            if info["status"] == "rejected"
                && let Some(t) = info["resetsAt"].as_i64()
            {
                until = until.max(Some(t));
            }
        }
    }
    // Claude Code's older answer gives its reset after a bar:
    // "Claude AI usage limit reached|1730000000".
    if until.is_none()
        && let Some((_, t)) = said.rsplit_once('|')
    {
        until = t.trim().parse().ok();
    }
    Some((
        until.or_else(|| claude_limit::resets(said, now)),
        said.to_owned(),
    ))
}

/// Claude Code's words for a spent plan, when its result doesn't carry the
/// API's error: "You've hit your monthly spend limit", "Claude AI usage limit
/// reached", "5-hour limit reached ∙ resets 3pm", "You've reached your usage
/// limit".
fn limit_words(said: &str) -> bool {
    let s = said.to_lowercase();
    s.contains("limit reached")
        || ((s.starts_with("you've hit your") || s.starts_with("you've reached your"))
            && s.contains("limit"))
}

#[cfg(test)]
mod tests;
