use super::*;

#[test]
fn opt_in_and_pipeline_scope_gate_the_real_drain() {
    let mut b = Box::new();
    let game = found("sam/game", 4, "sam", &["ready"], "2026-10-01T00:00:00Z");
    let pipeline = found("sam/box", 8, "sam", &["ready"], "2026-11-01T00:00:00Z");
    let settings = burrow_settings::Config {
        overlay: b.path("settings-overlay.json"),
        allowed: vec!["sam/box".into()],
        pipeline_repos: vec!["sam/box".into()],
        ..Default::default()
    };
    b.cfg.settings = Some(settings.clone());
    let t = b.cfg.inbox.as_ref().unwrap().triage.clone().unwrap();
    assert!(
        b.pass()
            .pick_ready(&t, std::slice::from_ref(&game))
            .is_none()
    );
    settings
        .command(&["allow".into(), "sam/game".into()], "test")
        .unwrap();
    assert!(
        b.pass()
            .pick_ready(&t, std::slice::from_ref(&game))
            .is_some()
    );
    let until = crate::now() + 3600;
    settings
        .command(&["pipeline".into(), until.to_string()], "test")
        .unwrap();
    b.pass().note_ready("sam/game", &[game]).unwrap();
    let mut held = b.pass();
    held.drain().unwrap();
    assert!(
        held.wake.values().any(|t| *t == until),
        "scope expiry books a pass with no eligible candidates"
    );
    assert!(!b.log().contains("labels[]=codex"));
    b.pass()
        .note_ready("sam/box", std::slice::from_ref(&pipeline))
        .unwrap();
    on_github(&b, &pipeline);
    b.pass().drain().unwrap();
    assert!(
        b.log().contains("issues/8/labels -f labels[]=codex"),
        "{}",
        b.log()
    );
    assert!(!b.log().contains("repos/sam/game/issues/4"), "{}", b.log());
    assert!(b.cfg.paused("sam/new"));
}

#[test]
fn an_unlisted_repo_can_still_be_triaged() {
    let mut b = Box::new();
    b.cfg.settings = Some(burrow_settings::Config {
        overlay: b.path("settings-overlay.json"),
        ..Default::default()
    });
    assert!(b.cfg.paused("sam/box"));
    let mut p = b.pass();
    p.triage("sam/box", &[issue(7, "sam", &[])]).unwrap();
    assert!(
        p.wake.contains_key("sam/box"),
        "triage still attempts sorting with the stand-in launcher"
    );
}
