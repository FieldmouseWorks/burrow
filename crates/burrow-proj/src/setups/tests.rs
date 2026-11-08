use super::*;
fn run(store: &Store<'_>, words: &[&str]) -> Result<String, String> {
    store.command(&words.iter().map(|s| s.to_string()).collect::<Vec<_>>())
}
#[test]
fn switches_back_and_refuses_bad_or_unknown_without_changing_link() {
    let d = tempfile::tempdir().unwrap();
    let roles = d.path().join("roles");
    fs::write(&roles, "work claude\n").unwrap();
    let store = Store::new(&roles);
    run(&store, &["save", "first"]).unwrap();
    fs::write(
        store.dir.join("second"),
        "work codex:chosen over 80 claude:other\nwork@sam/code claude\n",
    )
    .unwrap();
    let said = run(&store, &["use", "second"]).unwrap();
    assert!(said.contains("Was active: first\nActive: second\nPrevious: first"));
    assert!(
        said.contains("work codex over 80 claude")
            && !said.contains("chosen")
            && !said.contains("other")
    );
    assert!(roles.is_symlink());
    run(&store, &["use", "back"]).unwrap();
    assert_eq!(fs::read_to_string(&roles).unwrap(), "work claude\n");
    let link = fs::read_link(&roles).unwrap();
    let previous = fs::read(&store.previous).unwrap();
    fs::write(store.dir.join("bad"), "# header\njudge old-format\n").unwrap();
    let bad = run(&store, &["use", "bad"]).unwrap_err();
    assert!(bad.contains("roles.d/bad line 2"), "{bad}");
    for name in ["unknown", "../first", "", "UPPER"] {
        assert!(run(&store, &["use", name]).is_err());
    }
    assert_eq!(fs::read_link(&roles).unwrap(), link);
    assert_eq!(fs::read(&store.previous).unwrap(), previous);
}
#[test]
fn keeps_unsaved_plain_file_and_save_needs_replace() {
    let d = tempfile::tempdir().unwrap();
    let roles = d.path().join("roles");
    let store = Store::new(&roles);
    fs::write(&roles, "work codex\n").unwrap();
    run(&store, &["save", "saved"]).unwrap();
    assert!(run(&store, &["save", "saved"]).is_err());
    run(&store, &["save", "saved", "--replace"]).unwrap();
    fs::write(&roles, "work claude\n").unwrap();
    run(&store, &["use", "saved"]).unwrap();
    let old = store.previous().unwrap().unwrap();
    assert!(old.starts_with("unsaved-") && valid_name(&old), "{old}");
    assert_eq!(
        fs::read_to_string(store.dir.join(old)).unwrap(),
        "work claude\n"
    );
    run(&store, &["use", "back"]).unwrap();
    assert_eq!(fs::read_to_string(&roles).unwrap(), "work claude\n");
}
#[test]
fn config_check_rejects_old_format_with_file_and_line() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg: crate::config::Config =
        serde_json::from_value(crate::config::tests::example()).unwrap();
    cfg.roles = d.path().join("roles");
    fs::write(&cfg.roles, "# old file\njudge old-format\n").unwrap();
    let failure = crate::cmd::dispatch(&cfg, &["check-config".into()]).unwrap_err();
    assert_eq!(failure.code, 1);
    let error = failure.say.unwrap();
    assert!(
        error.contains("roles line 2") && error.contains("unknown engine"),
        "{error}"
    );
}
