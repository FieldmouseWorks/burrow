//! Claude stream and usage contracts, including empty resume results.

use super::{Refusal, claude_limit, is_error};
use serde_json::Value;

/// Whether a run ended refused, for usage or for its login. For usage, when
/// Claude Code says the limit resets (None when it doesn't) and a
/// normalized limit name. `last` is the run's result; `stream` its lines,
/// where the reset time is as a timestamp; failing that, its words say it
/// ([`crate::limit`]), read at `now`. For its login, the stream's own
/// `error` says so (`authentication_failed`, on the message Claude Code
/// makes of the API's refusal): an OAuth session that couldn't be
/// refreshed, or the API turning the credentials away.
pub fn refused(last: &Value, stream: &str, now: i64) -> Option<Refusal> {
    refused_records(
        last,
        stream
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok()),
        now,
    )
}

pub(super) fn refused_records(
    last: &Value,
    records: impl Iterator<Item = Value>,
    now: i64,
) -> Option<Refusal> {
    if !is_error(last) {
        return None;
    }
    let said = last["result"].as_str().unwrap_or_default().trim();
    let usage = last["api_error"] == "usage_limit_reached" || limit_words(said);
    let mut until = None;
    let mut named = None;
    let mut login = false;
    for v in records {
        login |= v["error"] == "authentication_failed";
        for info in [
            &v["api_error_params"]["rate_limit_info"],
            &v["rate_limit_info"],
        ] {
            if info["status"] == "rejected" {
                let name = match info["rateLimitType"].as_str() {
                    Some("seven_day") => "weekly allowance",
                    Some("five_hour") => "five-hour allowance",
                    _ if info["overageStatus"] == "rejected" => "extra-usage cap",
                    _ => "usage allowance",
                };
                if named.is_none() || info["resetsAt"].as_i64() > until {
                    named = Some(name);
                }
                until = until.max(info["resetsAt"].as_i64());
            }
        }
    }
    if !usage {
        return login.then(|| Refusal {
            until: None,
            said: if said.is_empty() {
                "Failed to authenticate".into()
            } else {
                said.to_owned()
            },
            login: true,
        });
    }
    // Claude Code's older answer gives its reset after a bar:
    // "Claude AI usage limit reached|1730000000".
    if until.is_none()
        && let Some((_, t)) = said.rsplit_once('|')
    {
        until = t.trim().parse().ok();
    }
    Some(Refusal::usage(
        until.or_else(|| claude_limit::resets(said, now)),
        format!(
            "Claude reached its {}",
            named.unwrap_or_else(|| limit_name(said))
        ),
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
pub(crate) mod tests;

/// Older streams have only words; never pass their billing instructions on.
fn limit_name(said: &str) -> &'static str {
    let s = said.to_lowercase();
    if s.contains("weekly") {
        "weekly allowance"
    } else if s.contains("5-hour") || s.contains("session limit") {
        "five-hour allowance"
    } else if s.contains("spend limit") {
        "extra-usage cap"
    } else {
        "usage allowance"
    }
}

/// Old holds contain the refusal verbatim. Preserve unrelated migration
/// descriptions, but remove billing instructions from recognized refusals.
pub(crate) fn normalize_refusal(said: &str) -> String {
    if limit_words(said) {
        format!("Claude reached its {}", limit_name(said))
    } else {
        said.to_owned()
    }
}
