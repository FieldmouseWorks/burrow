//! Every look at a PR resumes its session, on whichever head: told where it
//! last looked, and still the verdict on the whole change. A session is its
//! PR's alone, keeps the reviewers' login current, and goes for good once
//! its PR closes.

use std::fs;
use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};

use crate::rig::{HEAD, Rig};

/// A look at PR 5's `head`, queued and worked: the one run it took.
fn look_at(rig: &Rig, head: &str) -> Value {
    let snap = rig.snapshot(5, head);
    let entry = rig.queue(&snap, 5, head, json!({}));
    rig.agent(json!({}));
    rig.work();
    rig.back(&entry);
    let runs = rig.runs();
    let [run] = &runs[..] else { panic!("{runs:?}") };
    run.clone()
}

#[test]
fn each_look_resumes_the_last() {
    let rig = Rig::new();
    let first = look_at(&rig, HEAD);
    assert_eq!(first["resumed"], false);
    let newer = "f".repeat(40);
    let run = look_at(&rig, &newer);
    assert!(
        run["resumed"] == true && run["session"] == first["session"],
        "{run}"
    );
    let p = run["prompt"].as_str().unwrap();
    assert!(
        p.starts_with("This continues your review of this PR.")
            && p.contains("Review the whole change as it stands at HEAD"),
        "{p}"
    );
    assert!(
        p.contains(&format!(
            "Your last look was at {}, and the PR's head is now {}.",
            &HEAD[..12],
            &newer[..12]
        )),
        "{p}"
    );
    assert!(
        p.ends_with("\n\nYou review pull request #5 in sam/r."),
        "{p}"
    );
    let a: Vec<&str> = run["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(&a[..2], ["exec", "resume"]);
    assert_eq!(&a[a.len() - 2..], [first["session"].as_str().unwrap(), "-"]);
    assert!(!a.contains(&"--cd"), "{a:?}");
    let snap = rig.snapshot(5, &newer);
    assert_eq!(
        rig.look(&snap),
        json!({"id": first["session"], "head": newer})
    );
    // Again on the same head (its findings answered, say): the same session.
    let run = look_at(&rig, &newer);
    assert_eq!(run["session"], first["session"]);
    assert!(
        run["prompt"]
            .as_str()
            .unwrap()
            .contains("The PR's head is the one you last looked at."),
        "{run}"
    );
}

/// A session that won't resume (lost) fails that look, and the next starts
/// the PR's next session.
#[test]
fn a_session_that_wont_resume_starts_over() {
    let rig = Rig::new();
    let first = look_at(&rig, HEAD);
    let snap = rig.snapshot(5, HEAD);
    fs::remove_dir_all(rig.session(&snap).join("home/sessions")).unwrap();
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    rig.work();
    assert_eq!(rig.back(&entry)["result"], "error");
    assert!(!rig.session(&snap).join("look.json").exists());
    let run = look_at(&rig, HEAD);
    assert_eq!(run["resumed"], false);
    assert_ne!(run["session"], first["session"]);
    assert_eq!(rig.look(&snap)["id"], run["session"]);
}

/// Another PR's look has a session of its own, and never sees this one's.
#[test]
fn another_prs_look_has_its_own_session() {
    let rig = Rig::new();
    look_at(&rig, HEAD);
    let mine = rig.session(&rig.snapshot(5, HEAD));
    let other = rig.snapshot(6, HEAD);
    rig.boxes();
    let entry = rig.queue(&other, 6, HEAD, json!({}));
    rig.work();
    rig.back(&entry);
    let run = &rig.runs()[1];
    assert_eq!(run["resumed"], false);
    assert_eq!(
        run["env"]["CODEX_HOME"],
        rig.session(&other).join("home").to_str().unwrap()
    );
    let b = &rig.boxes()[0];
    let mine = mine.to_str().unwrap();
    assert!(
        !b["binds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x[1].as_str().unwrap().starts_with(mine)),
        "{b}"
    );
}

/// A refresh Codex saves as a new file in the PR's session goes back to
/// the reviewers' login, which every session links to; so does one saved
/// while its PR closed, its session marked gone under it. The machine's
/// settings for Codex are linked in too, and what the link points at is in
/// the sandbox (read-only), or it'd dangle there.
#[test]
fn a_refreshed_login_goes_back_to_every_session() {
    let rig = Rig::new();
    let login = rig.home().join(".codex");
    fs::write(
        login.join("auth.json"),
        r#"{"tokens": {"refresh_token": "old"}}"#,
    )
    .unwrap();
    fs::write(
        login.join("config.toml"),
        "model = \"set-by-the-machine\"\n",
    )
    .unwrap();
    let snap = rig.snapshot(5, HEAD);
    let link = rig.session(&snap).join("home/auth.json");
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    rig.agent(json!({"relogin": r#"{"tokens": {"refresh_token": "new"}}"#}));
    rig.boxes();
    rig.work();
    assert_eq!(rig.back(&entry)["result"], "review");
    assert_eq!(fs::read_link(&link).unwrap(), login.join("auth.json"));
    assert_eq!(
        fs::read_to_string(login.join("auth.json")).unwrap(),
        r#"{"tokens": {"refresh_token": "new"}}"#
    );
    assert_eq!(
        fs::metadata(login.join("auth.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_link(link.with_file_name("config.toml")).unwrap(),
        login.join("config.toml")
    );
    let b = &rig.boxes()[0];
    let auth = login.join("auth.json").to_string_lossy().into_owned();
    let config = login.join("config.toml").to_string_lossy().into_owned();
    let binds = b["binds"].as_array().unwrap();
    assert!(binds.contains(&json!(["--bind", auth, auth])), "{b}");
    assert!(binds.contains(&json!(["--ro-bind", config, config])), "{b}");

    rig.queue(&snap, 5, HEAD, json!({}));
    rig.agent(json!({"relogin": r#"{"tokens": {"refresh_token": "newer"}}"#, "gone": true}));
    rig.work();
    assert_eq!(
        fs::read_to_string(login.join("auth.json")).unwrap(),
        r#"{"tokens": {"refresh_token": "newer"}}"#
    );
    assert!(!rig.session(&snap).exists(), "the gone session is kept");
    assert!(
        rig.names("sessions").is_empty(),
        "{:?}",
        rig.names("sessions")
    );
}

/// A PR that closes (or is retargeted) takes its sessions with it, for
/// good: the sweep marks them gone, and the service deletes them, however
/// the agent left their modes. The rest stay.
#[test]
fn a_closed_prs_sessions_go_for_good() {
    let rig = Rig::new();
    let sessions = rig.path("sessions");
    for x in [
        "codex--pr-sam_r-5-1234abcd",
        ".gone-1-codex--pr-sam_r-6-1234abcd",
        ".gone-2-codex--pr-sam_r-5-99999999",
    ] {
        fs::create_dir_all(sessions.join(x).join("home/sessions")).unwrap();
        fs::write(sessions.join(x).join("home/sessions/s"), "said").unwrap();
    }
    let locked = sessions.join(".gone-1-codex--pr-sam_r-6-1234abcd/home");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
    rig.work();
    assert_eq!(rig.names("sessions"), ["codex--pr-sam_r-5-1234abcd"]);
}
