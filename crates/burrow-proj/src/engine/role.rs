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

/// Whether a turn moves from FROM's engine to TO's at PCT: FROM's is
/// short by its fresh meter (any window at PCT, and no credits), or, for
/// Claude, the machine is out of Claude's usage (`held`, usage.rs) before a
/// meter could say so; and TO's has room by its own fresh meter (credits,
/// or no window used up). A missing, malformed, future or hour-old reading
/// moves nothing: it can't say the move helps.
fn over(meters: Option<&Path>, held: bool, from: Engine, to: Engine, pct: f64, now: i64) -> bool {
    use burrow_usage_reset::meter::Snapshot;
    let Some(dir) = meters else {
        return false;
    };
    let read = |e: Engine| Snapshot::read(dir, e.name(), now as f64);
    let short = (held && from == Engine::Claude) || read(from).is_some_and(|m| m.short(pct));
    short && read(to).is_some_and(|m| m.room())
}

pub(crate) fn role<'a>(
    line: &'a str,
    meters: Option<&Path>,
    held: bool,
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
            Some((pct, other)) if over(meters, held, primary.engine, other.engine, pct, now) => {
                other
            }
            _ => primary,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meter(dir: &Path, engine: &str, windows: &[(&str, f64)], credits: bool, at: i64) {
        let windows: serde_json::Map<_, _> = windows
            .iter()
            .map(|(n, u)| ((*n).to_owned(), json!({"used": u, "resets_at": 20000})))
            .collect();
        std::fs::write(
            dir.join(format!("{engine}.json")),
            json!({"windows": windows, "credits": credits, "read_at": at}).to_string(),
        )
        .unwrap();
    }

    fn pick(dir: &Path, held: bool) -> Engine {
        role("work claude over 80 codex", Some(dir), held, 10000)
            .unwrap()
            .1
            .engine
    }

    #[test]
    fn overflow_needs_its_engine_short_and_the_other_with_room() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        assert_eq!(pick(dir, false), Engine::Claude); // no meters
        meter(dir, "codex", &[("300m", 10.0)], false, 9999);
        for (windows, at, expected) in [
            (
                &[("seven_day", 79.0), ("five_hour", 50.0)][..],
                9999,
                Engine::Claude,
            ),
            (&[("seven_day", 80.0)][..], 6401, Engine::Codex),
            // The five-hour session counts as much as the week.
            (
                &[("seven_day", 20.0), ("five_hour", 95.0)][..],
                9999,
                Engine::Codex,
            ),
            // An hour old, or from the future: it moves nothing.
            (&[("seven_day", 100.0)][..], 6400, Engine::Claude),
            (&[("seven_day", 100.0)][..], 10001, Engine::Claude),
        ] {
            meter(dir, "claude", windows, false, at);
            assert_eq!(pick(dir, false), expected, "{windows:?} at {at}");
        }
        std::fs::write(dir.join("claude.json"), "broken").unwrap();
        assert_eq!(pick(dir, false), Engine::Claude);
    }

    #[test]
    fn credits_are_room_and_a_hold_is_short() {
        let dir = tempfile::tempdir().unwrap();
        let dir = dir.path();
        meter(dir, "claude", &[("seven_day", 99.0)], false, 9999);
        // Codex's week used up: no room, unless it has credits to spend.
        meter(
            dir,
            "codex",
            &[("300m", 3.0), ("10080m", 100.0)],
            false,
            9999,
        );
        assert_eq!(pick(dir, false), Engine::Claude);
        meter(
            dir,
            "codex",
            &[("300m", 3.0), ("10080m", 100.0)],
            true,
            9999,
        );
        assert_eq!(pick(dir, false), Engine::Codex);
        // Out of Claude's usage before its meter says so: moved too.
        meter(dir, "claude", &[("seven_day", 10.0)], false, 9999);
        assert_eq!(pick(dir, false), Engine::Claude);
        assert_eq!(pick(dir, true), Engine::Codex);
        // But never to an engine whose meter can't be read.
        std::fs::remove_file(dir.join("codex.json")).unwrap();
        assert_eq!(pick(dir, true), Engine::Claude);
        // A hold is Claude's: it doesn't make Codex short.
        meter(dir, "claude", &[("seven_day", 10.0)], false, 9999);
        meter(dir, "codex", &[("300m", 10.0)], false, 9999);
        let back = role("work codex over 80 claude", Some(dir), true, 10000)
            .unwrap()
            .1;
        assert_eq!(back.engine, Engine::Codex);
        meter(dir, "codex", &[("300m", 90.0)], false, 9999);
        let back = role("work codex over 80 claude", Some(dir), false, 10000)
            .unwrap()
            .1;
        assert_eq!(back.engine, Engine::Claude);
    }
}
