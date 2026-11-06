//! Codex CLI refusals, shared by the reviewer and project jobs.

/// When a usage limit says to try again, in seconds: "try again in 2 days 3
/// hours", else an hour on; never less than a minute.
pub fn wait_for(text: &str) -> f64 {
    const AT: &str = "try again in ";
    let Some(i) = text.find(AT) else {
        return 3600.0;
    };
    let mut rest = &text[i + AT.len()..];
    let mut total: Option<u64> = None;
    loop {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let Ok(n) = rest[..digits].parse::<u64>() else {
            break;
        };
        let after = rest[digits..].trim_start();
        let word = after.len()
            - after
                .trim_start_matches(|c: char| c.is_ascii_lowercase())
                .len();
        let unit = match &after[..word] {
            "day" | "days" => 86400,
            "hour" | "hours" => 3600,
            "minute" | "minutes" => 60,
            "second" | "seconds" => 1,
            _ => break,
        };
        total = Some(total.unwrap_or(0).saturating_add(n.saturating_mul(unit)));
        rest = after[word..].trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        if let Some(more) = rest.strip_prefix("and")
            && more.starts_with(char::is_whitespace)
        {
            rest = more.trim_start();
        }
    }
    total.map_or(3600.0, |t| t.max(60) as f64)
}

/// Whether text has `word` with no letter, digit or underscore either side.
fn has_word(text: &str, word: &str) -> bool {
    let wordy = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    text.match_indices(word).any(|(i, _)| {
        !wordy(text[..i].chars().next_back()) && !wordy(text[i + word.len()..].chars().next())
    })
}

/// The plan's usage spent.
pub fn spent(said: &str) -> bool {
    let said = said.to_lowercase();
    said.contains("usage limit") || said.contains("usage_limit_reached")
}

/// Its login gone or refused.
pub fn logged_out(said: &str) -> bool {
    let said = said.to_lowercase();
    [
        "not logged in",
        "login again",
        "log in again",
        "unauthorized",
        "refresh token",
    ]
    .iter()
    .any(|x| said.contains(x))
        || has_word(&said, "401")
}

#[cfg(test)]
mod tests {
    use super::{has_word, logged_out, spent, wait_for};
    /// A usage limit's own word for when, or an hour; a minute at least.
    #[test]
    fn when_a_limit_says_to_try_again() {
        assert_eq!(
            wait_for("Upgrade to Pro or try again in 2 days 3 hours."),
            (2.0 * 86400.0) + 3.0 * 3600.0
        );
        assert_eq!(wait_for("try again in 20 minutes"), 1200.0);
        assert_eq!(wait_for("try again in 1 hour, and 30 minutes"), 5400.0);
        assert_eq!(wait_for("try again in 1 day and 2 hours"), 93600.0);
        assert_eq!(wait_for("try again in 5 seconds"), 60.0);
        assert_eq!(wait_for("try again in 2days"), 172800.0);
        for none in [
            "try again later",
            "try again in a bit",
            "Try again in 2 hours",
            "",
        ] {
            assert_eq!(wait_for(none), 3600.0, "{none}");
        }
    }

    /// Spent and logged out by what codex says; a 401 inside a longer
    /// number is neither.
    #[test]
    fn out_of_service_by_its_own_words() {
        assert!(spent("You've hit your usage limit.") && spent("error: USAGE_LIMIT_REACHED"));
        assert!(!spent("no usage of limits"));
        for out in [
            "Error: Not logged in.",
            "please log in again",
            "Login again",
            "HTTP 401",
            "401: Unauthorized",
            "bad refresh token",
        ] {
            assert!(logged_out(out), "{out}");
        }
        for not in ["no session session-1793040155", "x401", "4010", "_401"] {
            assert!(!logged_out(not), "{not}");
        }
        assert!(has_word("(401)", "401") && has_word("401", "401"));
    }
}
