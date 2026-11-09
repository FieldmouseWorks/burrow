//! Persist attribution while the authenticated issue snapshot is in hand.
use serde_json::Value;
pub(super) fn kind(steward: Option<&str>, issue: &Value) -> String {
    let opener = issue["user"]["login"].as_str().unwrap_or_default();
    let who = if steward.is_some_and(|m| opener.eq_ignore_ascii_case(m)) {
        "steward App"
    } else if opener.ends_with("[bot]") || issue["user"]["type"] == "Bot" {
        "another App"
    } else if opener.is_empty() {
        "unknown"
    } else {
        "owner"
    };
    let mut labels: Vec<_> = issue["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["name"].as_str())
        .collect();
    labels.sort_unstable();
    labels.dedup();
    format!("inbox labels={} opener={who}", labels.join(","))
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn attribution_retains_labels_and_opener_kind_only() {
        for (login, expected) in [
            ("sam", "owner"),
            ("steward[bot]", "steward App"),
            ("workers[bot]", "another App"),
        ] {
            let got = kind(
                Some("steward[bot]"),
                &json!({"user":{"login":login},"labels":[{"name":"codex"},{"name":"ready"}],"title":"private", "body":"private"}),
            );
            assert_eq!(got, format!("inbox labels=codex,ready opener={expected}"));
        }
    }
}
