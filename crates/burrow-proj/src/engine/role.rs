//! Role grammar and the fresh weekly reading that may select overflow.

use super::{Choice, Engine};
use std::path::Path;

/// A scope is one GitHub repo, folded once for matching and duplicate keys.
/// Role names keep their spelling; only the repo is case-insensitive.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct RoleKey<'a> {
    name: &'a str,
    repo: Option<String>,
}

impl<'a> RoleKey<'a> {
    fn parse(key: &'a str) -> Result<Self, String> {
        let (name, repo) = key
            .split_once('@')
            .map_or((key, None), |(name, repo)| (name, Some(repo)));
        if name.is_empty() || repo.is_some_and(|r| !burrow_github::repo_name(r)) {
            return Err("expected ROLE or ROLE@OWNER/REPO".into());
        }
        Ok(Self {
            name,
            repo: repo.map(str::to_ascii_lowercase),
        })
    }

    /// Scoped beats bare, and a different repo never matches. None is no match.
    pub(crate) fn specificity(&self, role: &str, repo: Option<&str>) -> Option<u8> {
        if self.name != role {
            return None;
        }
        match &self.repo {
            None => Some(0),
            Some(scope) if repo.is_some_and(|r| scope.eq_ignore_ascii_case(r)) => Some(1),
            Some(_) => None,
        }
    }
}

impl Choice {
    pub fn parse(s: &str) -> Result<Self, String> {
        let (engine, model) = s.split_once(':').map_or((s, None), |(e, m)| (e, Some(m)));
        let engine = match engine {
            "claude" => Engine::Claude,
            "codex" => Engine::Codex,
            _ => return Err(format!("unknown engine {engine}")),
        };
        if model.is_some_and(|m| {
            m.is_empty()
                || !m
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._:[]-".contains(&b))
        }) {
            return Err("invalid model choice".into());
        }
        Ok(Self {
            engine,
            model: model.map(str::to_owned),
        })
    }
}

/// A missing, malformed, future or hour-old reading cannot move a turn.
fn over(meter: Option<&Path>, pct: f64, now: i64) -> bool {
    let reading = meter
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice::<burrow_usage_reset::meter::Snapshot>(&b).ok());
    reading.is_some_and(|r| {
        r.read_at <= now as f64
            && (now as f64) - r.read_at < 3600.0
            && (0.0..=100.0).contains(&r.utilization)
            && r.utilization >= pct
    })
}

pub(crate) fn role<'a>(
    line: &'a str,
    meter: Option<&Path>,
    now: i64,
) -> Result<(RoleKey<'a>, Choice), String> {
    let words: Vec<_> = line.split_whitespace().collect();
    let (name, primary, overflow) = match words.as_slice() {
        [name, choice] => (*name, *choice, None),
        [name, choice, "over", pct, other] => {
            let pct: f64 = pct.parse().map_err(|_| "overflow percent isn't a number")?;
            if !(0.0..=100.0).contains(&pct) {
                return Err("overflow percent must be 0 to 100".into());
            }
            (*name, *choice, Some((pct, Choice::parse(other)?)))
        }
        _ => {
            return Err(
                "expected ROLE[@OWNER/REPO] ENGINE[:MODEL] [over PCT ENGINE[:MODEL]]".into(),
            );
        }
    };
    let primary = Choice::parse(primary)?;
    Ok((
        RoleKey::parse(name)?,
        match overflow {
            Some((pct, other)) if over(meter, pct, now) => other,
            _ => primary,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overflow_requires_the_line_and_a_fresh_reading_at_the_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("meter.json");
        let pick = |at| {
            role("work claude over 80 codex", Some(&path), at)
                .unwrap()
                .1
                .engine
        };
        assert_eq!(pick(10000), Engine::Claude); // missing
        for (used, at, expected) in [
            (79, 9999, Engine::Claude),
            (80, 6401, Engine::Codex),
            (100, 6400, Engine::Claude),
            (100, 10001, Engine::Claude),
        ] {
            std::fs::write(
                &path,
                serde_json::json!({"utilization": used, "resets_at": "2026-11-07T12:00:00Z", "read_at": at})
                    .to_string(),
            )
            .unwrap();
            assert_eq!(pick(10000), expected);
        }
        std::fs::write(&path, "broken").unwrap();
        assert_eq!(pick(10000), Engine::Claude);
    }
}
