//! What ran, held to what the plugin offers and the flow chose, against the
//! scripted judges and the scripted GitHub.
use super::world::world;
use super::{Scripted, answer, ask, judge_request, producer};
use conary_core::plugin::{Context, Executed, Judgment, Trigger};
use serde_json::json;

#[test]
fn what_ran_must_be_what_the_plugin_offers_and_the_flow_chose() {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    let ran = |engine: &str, model: Option<&str>, effort: Option<&str>| {
        let mut a = answer("judge-1", Judgment::Pass);
        a.by.engine = engine.into();
        a.execution = Some(Executed {
            model: model.map(Into::into),
            effort: effort.map(Into::into),
            context: Context::Fresh,
        });
        a
    };
    let judges = Scripted::answering(vec![
        ran("codex", Some("a-model"), Some("high")),
        ran("claude", Some("b-model"), Some("high")),
        ran("claude", Some("a-model"), Some("low")),
        ran("claude", None, None),
    ]);
    let p = producer(dir.path(), &w, judges);
    let request = judge_request(&w, vec![Trigger::Asked]);
    for i in 0..4 {
        let got = ask(dir.path(), &p, &request);
        assert!(got.is_err(), "{i}: {got:?}");
    }
    assert!(w.posted().is_empty());
    assert!(w.with(|p| p.runs.is_empty()));
    // With no choice, an offered model and effort is still required.
    let mut unchosen = judge_request(&w, vec![Trigger::Asked]);
    unchosen.execution.as_mut().unwrap().choice = None;
    let judges = Scripted::answering(vec![ran("claude", Some("b-model"), None)]);
    let p = producer(dir.path(), &w, judges);
    assert!(ask(dir.path(), &p, &unchosen).is_err());
    assert!(w.posted().is_empty());
}

#[test]
fn what_ran_is_held_to_what_was_offered_when_the_request_came() {
    let dir = tempfile::tempdir().unwrap();
    let roles = dir.path().join("overlay.roles.json");
    // The settings as they're changed while a job judges: a model or an
    // effort newly offered.
    let model = json!({"routing": {"models": {"judge claude": {"model": "b-model", "effort": "high"}}},
        "offers": {"claude": {"models": ["a-model", "b-model"], "efforts": ["high"]}}, "migrated": 1});
    let effort = json!({"routing": {"models": {"judge claude": {"model": "a-model", "effort": "low"}}},
        "offers": {"claude": {"models": ["a-model"], "efforts": ["high", "low"]}}, "migrated": 1});
    let ran = |engine: &str, model: &str, effort: &str| {
        let mut a = answer("judge-1", Judgment::Pass);
        a.by.engine = engine.into();
        a.execution = Some(Executed {
            model: Some(model.into()),
            effort: Some(effort.into()),
            context: Context::Fresh,
        });
        a
    };
    // No choice, and a choice of the model alone.
    let w = world();
    let mut unchosen = judge_request(&w, vec![Trigger::Asked]);
    unchosen.execution.as_mut().unwrap().choice = None;
    let mut model_only = judge_request(&w, vec![Trigger::Asked]);
    model_only
        .execution
        .as_mut()
        .unwrap()
        .choice
        .as_mut()
        .unwrap()
        .effort = None;
    let chosen = judge_request(&w, vec![Trigger::Asked]);
    // Each row: the request, the settings written while its job judges,
    // what ran, why it's refused, and whether a job was asked first.
    for (request, settings, answered, why, asked) in [
        // A request whose choice names no model is refused before any work:
        // the offer read when it came may already not be the one the flow
        // negotiated.
        (
            &unchosen,
            &model,
            ran("claude", "b-model", "high"),
            "chose no model",
            false,
        ),
        (
            &unchosen,
            &effort,
            ran("claude", "a-model", "low"),
            "chose no model",
            false,
        ),
        // So is one that names the model alone while the role runs at an
        // effort: it would hold what ran to no effort.
        (
            &model_only,
            &effort,
            ran("claude", "a-model", "low"),
            "the flow chose",
            false,
        ),
        // One the role runs exactly is asked, and what ran at an effort the
        // offer read when the request came didn't take is refused after.
        (
            &chosen,
            &effort,
            ran("claude", "a-model", "low"),
            "plugin offers",
            true,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let w = world();
        let judges = Scripted::answering(vec![answered.clone()]);
        let (roles, settings) = (dir.path().join("overlay.roles.json"), settings.to_string());
        let p = producer(dir.path(), &w, judges.clone());
        judges.0.lock().unwrap().during =
            Some(Box::new(move || std::fs::write(&roles, settings).unwrap()));
        let refused = ask(dir.path(), &p, request).unwrap_err();
        assert!(refused.contains(why), "{answered:?}: {refused}");
        assert_eq!(
            !judges.0.lock().unwrap().asked.is_empty(),
            asked,
            "{answered:?}"
        );
        assert!(w.posted().is_empty(), "{answered:?}");
        assert!(w.with(|p| p.runs.is_empty()), "{answered:?}");
    }
    // What ran as the flow chose still goes out, the settings changed or
    // not: Conary held the choice to the offers it negotiated.
    let judges = Scripted::answering(vec![answer("judge-1", Judgment::Pass)]);
    let p = producer(dir.path(), &w, judges.clone());
    let changed = model.to_string();
    judges.0.lock().unwrap().during =
        Some(Box::new(move || std::fs::write(&roles, changed).unwrap()));
    ask(dir.path(), &p, &judge_request(&w, vec![Trigger::Asked])).unwrap();
    assert_eq!(w.posted().len(), 1);
}
