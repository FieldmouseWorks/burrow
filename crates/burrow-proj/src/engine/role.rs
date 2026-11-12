//! The engine a role's route picks, with the fresh reading that may move it.

use super::Engine;
use burrow_settings::roles::Route;
use std::path::Path;

/// Whether a turn moves from FROM's engine to TO's at PCT: FROM's is
/// short by its fresh meter (any window at PCT, and no credits), or, for
/// Claude, the machine is out of Claude's usage (`held`, usage.rs) before a
/// meter could say so; and TO's has room by its own fresh meter (credits,
/// or no window used up), which Claude out of usage hasn't, whatever its
/// last reading said. A missing, malformed, future or hour-old reading
/// moves nothing: it can't say the move helps.
fn over(meters: Option<&Path>, held: bool, from: Engine, to: Engine, pct: f64, now: i64) -> bool {
    use burrow_usage_reset::meter::Snapshot;
    let Some(dir) = meters else {
        return false;
    };
    let read = |e: Engine| Snapshot::read(dir, e.name(), now as f64);
    let out = |e: Engine| held && e == Engine::Claude;
    let short = out(from) || read(from).is_some_and(|m| m.short(pct));
    short && !out(to) && read(to).is_some_and(|m| m.room())
}

/// The engine ROUTE runs a turn on now: its own, or its overflow's when
/// [`over`] says the move helps.
pub(crate) fn route(
    route: &Route,
    meters: Option<&Path>,
    held: bool,
    now: i64,
) -> Result<Engine, String> {
    let primary = Engine::parse(&route.engine)?;
    Ok(match &route.over {
        Some(o) => {
            let other = Engine::parse(&o.engine)?;
            if over(meters, held, primary, other, o.pct, now) {
                other
            } else {
                primary
            }
        }
        None => primary,
    })
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

    fn role(words: &str, dir: &Path, held: bool) -> Engine {
        let words: Vec<_> = words.split_whitespace().collect();
        let r = burrow_settings::roles::route(&words).unwrap();
        route(&r, Some(dir), held, 10000).unwrap()
    }

    fn pick(dir: &Path, held: bool) -> Engine {
        role("claude over 80 codex", dir, held)
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
        let back = role("codex over 80 claude", dir, true);
        assert_eq!(back, Engine::Codex);
        meter(dir, "codex", &[("300m", 90.0)], false, 9999);
        let back = role("codex over 80 claude", dir, false);
        assert_eq!(back, Engine::Claude);
        // Nor does it move a turn onto a Claude that's out, whatever its
        // last reading said.
        let back = role("codex over 80 claude", dir, true);
        assert_eq!(back, Engine::Codex);
    }
}
