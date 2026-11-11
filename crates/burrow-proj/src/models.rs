//! Which model and effort each role's turns run at, on each engine: the
//! models file beside the roles file. The roles file picks the engine (and
//! a saved engine setup swaps it whole); this one tunes each engine, so a
//! switch of setups keeps it. A role with no line, or a line's `-` model,
//! keeps the engine's own setting (Claude's settings.json, Codex's config).

use crate::engine::{Engine, RoleKey};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Tuning {
    pub model: Option<String>,
    pub effort: Option<String>,
}

/// The levels each engine's CLI takes. Claude Code's `--effort` has a fixed
/// set; Codex's `model_reasoning_effort` also takes levels its models define
/// (the pinned CLI's `ReasoningEffort` parser), so any plain word goes
/// through unchanged and Codex itself says when a model lacks it.
fn effort_ok(engine: Engine, effort: &str) -> bool {
    match engine {
        Engine::Claude => ["low", "medium", "high", "xhigh", "max"].contains(&effort),
        Engine::Codex => {
            !effort.is_empty()
                && effort
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
        }
    }
}

fn model_ok(model: &str) -> bool {
    !model.is_empty()
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:[]-".contains(&b))
}

fn line(text: &str) -> Result<(RoleKey<'_>, Engine, Tuning), String> {
    let words: Vec<_> = text.split_whitespace().collect();
    let (key, engine, model, effort) = match words.as_slice() {
        [key, engine, model] => (*key, *engine, *model, None),
        [key, engine, model, effort] => (*key, *engine, *model, Some(*effort)),
        _ => return Err("expected ROLE[@OWNER/REPO] ENGINE MODEL|- [EFFORT]".into()),
    };
    let engine = Engine::parse(engine)?;
    let model = (model != "-").then_some(model);
    if model.is_some_and(|m| !model_ok(m)) {
        return Err("invalid model choice".into());
    }
    if let Some(e) = effort.filter(|e| !effort_ok(engine, e)) {
        return Err(format!("{} takes no effort {e}", engine.name()));
    }
    Ok((
        RoleKey::parse(key)?,
        engine,
        Tuning {
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
        },
    ))
}

/// ROLE's model and effort on ENGINE in REPO. Every non-comment line must
/// parse, so a typo never silently runs a role on the wrong model; a repo's
/// line beats the bare one; two lines for one role and engine fail with
/// both named, even on other roles. An absent file tunes nothing, and so does
/// `ROLE ENGINE -`, which lets a repo's line put back the engine's own
/// settings over a bare line's.
pub(crate) fn tuning(
    path: &Path,
    role: &str,
    repo: Option<&str>,
    engine: Engine,
) -> Result<Tuning, String> {
    let text = crate::roles::read(path)?;
    let mut entries = BTreeMap::new();
    for (i, text) in text.lines().enumerate() {
        if text.trim().is_empty() || text.trim_start().starts_with('#') {
            continue;
        }
        let (key, on, tuning) =
            line(text).map_err(|e| format!("{} line {}: {text}: {e}", path.display(), i + 1))?;
        let key = (key, on);
        if let Some((first, original, _)) = entries.get(&key) {
            return Err(format!(
                "{}: duplicate models key: line {first}: {original}; line {}: {text}",
                path.display(),
                i + 1
            ));
        }
        entries.insert(key, (i + 1, text, tuning));
    }
    Ok(entries
        .into_iter()
        .filter(|((_, on), _)| *on == engine)
        .filter_map(|((key, _), (_, _, tuning))| {
            key.specificity(role, repo).map(|rank| (rank, tuning))
        })
        .max_by_key(|(rank, _)| *rank)
        .map(|(_, tuning)| tuning)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tune(model: Option<&str>, effort: Option<&str>) -> Tuning {
        Tuning {
            model: model.map(str::to_owned),
            effort: effort.map(str::to_owned),
        }
    }

    #[test]
    fn each_engine_has_its_own_line_and_a_repo_beats_bare() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models");
        let get = |role, repo, engine| tuning(&path, role, repo, engine).unwrap();
        // No file: nothing tuned.
        assert_eq!(get("fix", None, Engine::Claude), Tuning::default());
        std::fs::write(
            &path,
            "# tuning\nfix claude small medium\nfix codex - low\nfix@Sam/Code claude big\njudge claude - max\n",
        )
        .unwrap();
        assert_eq!(
            get("fix", None, Engine::Claude),
            tune(Some("small"), Some("medium"))
        );
        assert_eq!(get("fix", None, Engine::Codex), tune(None, Some("low")));
        assert_eq!(
            get("fix", Some("sam/code"), Engine::Claude),
            tune(Some("big"), None)
        );
        assert_eq!(
            get("fix", Some("sam/other"), Engine::Claude),
            tune(Some("small"), Some("medium"))
        );
        assert_eq!(get("judge", None, Engine::Claude), tune(None, Some("max")));
        assert_eq!(get("judge", None, Engine::Codex), Tuning::default());
        assert_eq!(get("other", None, Engine::Claude), Tuning::default());
        // A repo's `-` line puts back the engine's own settings, in either
        // order and any case; Codex takes the levels its models define.
        for text in [
            "fix claude small medium\nfix@Sam/Code claude -\nfix codex - max\nfix@sam/code codex - ultra\n",
            "fix@sam/code codex - ultra\nfix@SAM/code claude -\nfix codex - max\nfix claude small medium\n",
        ] {
            std::fs::write(&path, text).unwrap();
            assert_eq!(
                get("fix", Some("sam/code"), Engine::Claude),
                Tuning::default()
            );
            assert_eq!(
                get("fix", Some("sam/other"), Engine::Claude),
                tune(Some("small"), Some("medium"))
            );
            assert_eq!(get("fix", None, Engine::Codex), tune(None, Some("max")));
            assert_eq!(
                get("fix", Some("sam/code"), Engine::Codex),
                tune(None, Some("ultra"))
            );
        }
        std::fs::write(&path, "fix@sam/code claude -\nfix@Sam/Code claude big\n").unwrap();
        assert!(
            tuning(&path, "fix", None, Engine::Claude)
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn a_bad_or_duplicate_line_fails_every_role() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("models");
        for bad in [
            "fix",
            "fix claude",
            "fix other small",
            "fix claude a$b",
            "fix claude small huge",
            "fix codex small Max",
            "fix codex small a=b",
            "fix claude small medium extra",
            "fix@owner claude small",
        ] {
            std::fs::write(&path, format!("# tuning\n{bad}\n")).unwrap();
            let error = tuning(&path, "other", None, Engine::Claude).unwrap_err();
            assert!(error.contains("line 2") && error.contains(bad), "{error}");
        }
        std::fs::write(&path, "fix claude a\nfix codex b\nfix claude - low\n").unwrap();
        let error = tuning(&path, "other", None, Engine::Codex).unwrap_err();
        assert!(
            error.contains("line 1: fix claude a") && error.contains("line 3: fix claude - low"),
            "{error}"
        );
        // An unreadable path is never "nothing tuned".
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(tuning(&path, "fix", None, Engine::Claude).is_err());
    }
}
