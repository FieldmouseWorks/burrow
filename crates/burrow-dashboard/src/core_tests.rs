//! The ceremonies end to end, with a software authenticator standing in for
//! the owner's devices.

use super::*;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;

const ORIGIN: &str = "https://box.tail1.ts.net";
const HOUR: u64 = 3600;

fn config(dir: &std::path::Path) -> Config {
    Config {
        title: "box".into(),
        origin: ORIGIN.into(),
        port: 0,
        state: dir.into(),
        shell: dir.into(),
        fonts: dir.into(),
        owner: Owner {
            id: 583231,
            login: "sam".into(),
        },
        idle_timeout: HOUR,
        max_age: 24 * HOUR,
    }
}

fn origin() -> Url {
    Url::parse(ORIGIN).unwrap()
}

/// A device: an authenticator that verifies its user.
fn device() -> SoftPasskey {
    SoftPasskey::new(true)
}

/// Enrol `dev` as `name` with a fresh code: the session's token.
fn enrol(core: &mut Core, dev: &mut SoftPasskey, name: &str, now: u64) -> Result<String> {
    let code = core.codes().issue(now).unwrap();
    let (id, options) = core.enrol_start(&code, name, now)?;
    let answer = dev.do_registration(origin(), options).unwrap();
    core.enrol_finish(&id, &answer, now)
}

fn login(core: &mut Core, dev: &mut SoftPasskey, now: u64) -> Result<String> {
    let (id, options) = core.login_start(now)?;
    let answer = dev.do_authentication(origin(), options).unwrap();
    core.login_finish(&id, &answer, now)
}

#[test]
fn a_device_enrols_with_the_code_and_signs_in_with_its_passkey() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut phone = device();

    let first = enrol(&mut core, &mut phone, "  Phone ", 100).unwrap();
    assert_eq!(core.passkeys().len(), 1);
    assert_eq!(core.passkeys()[0].name, "Phone");
    // Enrolling signs the device in.
    assert!(core.session(&first, 101).unwrap().is_some());

    let token = login(&mut core, &mut phone, 200).unwrap();
    let live = core.session(&token, 201).unwrap().unwrap();
    assert_eq!(live.session.passkey, "Phone");
    assert_eq!(live.ends, 201 + HOUR);
    assert_eq!(core.passkeys()[0].used, Some(200));

    // Passkeys and sessions outlive the process.
    let mut again = Core::open(&config(dir.path())).unwrap();
    assert_eq!(again.passkeys().len(), 1);
    assert!(again.session(&token, 300).unwrap().is_some());
    login(&mut again, &mut phone, 400).unwrap();
}

#[test]
fn enrolment_refuses_a_wrong_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    assert!(matches!(
        core.enrol_start("ABCDE-FGHJK", "", 0),
        Err(Error::NoCode)
    ));
    core.codes().issue(0).unwrap();
    assert!(matches!(
        core.enrol_start("ABCDE-FGHJK", "", 1),
        Err(Error::WrongCode { left: 4 })
    ));
    assert!(core.pending.is_empty());
    assert!(core.passkeys().is_empty());
}

#[test]
fn each_code_enrols_one_device_and_the_owner_may_have_several() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let (mut laptop, mut phone) = (device(), device());

    let code = core.codes().issue(0).unwrap();
    let (id, options) = core.enrol_start(&code, "Laptop", 1).unwrap();
    // A second ceremony on the same code, before the first is answered...
    let (id2, options2) = core.enrol_start(&code, "Phone", 1).unwrap();
    let answer = laptop.do_registration(origin(), options).unwrap();
    core.enrol_finish(&id, &answer, 2).unwrap();
    // ...can't keep its passkey: the code went to the first.
    let answer2 = phone.do_registration(origin(), options2).unwrap();
    assert!(matches!(
        core.enrol_finish(&id2, &answer2, 3),
        Err(Error::NoCode)
    ));
    assert!(matches!(
        core.enrol_start(&code, "Phone", 3),
        Err(Error::NoCode)
    ));

    // A fresh code adds the phone, and either device signs in.
    enrol(&mut core, &mut phone, "", 4).unwrap();
    let names: Vec<_> = core.passkeys().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Laptop", "Passkey 1"]);
    enrol(&mut core, &mut device(), "Laptop", 5).unwrap();
    assert_eq!(core.passkeys()[2].name, "Laptop 2");
    let t = login(&mut core, &mut laptop, 5).unwrap();
    assert_eq!(
        core.session(&t, 5).unwrap().unwrap().session.passkey,
        "Laptop"
    );
    let t = login(&mut core, &mut phone, 6).unwrap();
    assert_eq!(
        core.session(&t, 6).unwrap().unwrap().session.passkey,
        "Passkey 1"
    );
}

#[test]
fn a_new_code_mid_ceremony_voids_the_old_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut dev = device();
    let code = core.codes().issue(0).unwrap();
    let (id, options) = core.enrol_start(&code, "", 1).unwrap();
    core.codes().issue(2).unwrap();
    let answer = dev.do_registration(origin(), options).unwrap();
    assert!(matches!(
        core.enrol_finish(&id, &answer, 3),
        Err(Error::NoCode)
    ));
    assert!(core.passkeys().is_empty());
}

#[test]
fn enrolment_asks_devices_not_to_enrol_twice() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    enrol(&mut core, &mut device(), "One", 0).unwrap();
    let code = core.codes().issue(1).unwrap();
    let (_, options) = core.enrol_start(&code, "Two", 1).unwrap();
    let excluded: Vec<_> = options
        .public_key
        .exclude_credentials
        .unwrap_or_default()
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(excluded, [core.passkeys()[0].passkey.cred_id().clone()]);
}

#[test]
fn signing_in_needs_an_enrolled_passkey() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    assert!(matches!(core.login_start(0), Err(Error::NoPasskeys)));
    enrol(&mut core, &mut device(), "Mine", 0).unwrap();
    // Another authenticator has no credential the options allow.
    let (_, options) = core.login_start(1).unwrap();
    assert!(device().do_authentication(origin(), options).is_err());
}

#[test]
fn an_answer_counts_once_and_only_in_time() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut dev = device();
    enrol(&mut core, &mut dev, "", 0).unwrap();

    let (id, options) = core.login_start(10).unwrap();
    let answer = dev.do_authentication(origin(), options).unwrap();
    core.login_finish(&id, &answer, 11).unwrap();
    // Replayed, it's no ceremony.
    assert!(matches!(
        core.login_finish(&id, &answer, 12),
        Err(Error::NoCeremony)
    ));

    let (id, options) = core.login_start(20).unwrap();
    let answer = dev.do_authentication(origin(), options).unwrap();
    assert!(matches!(
        core.login_finish(&id, &answer, 20 + CEREMONY_TTL),
        Err(Error::NoCeremony)
    ));

    // An answer to one ceremony doesn't finish another.
    let (_, options) = core.login_start(30).unwrap();
    let (other, _) = core.login_start(30).unwrap();
    let answer = dev.do_authentication(origin(), options).unwrap();
    assert!(matches!(
        core.login_finish(&other, &answer, 31),
        Err(Error::Rejected(_))
    ));
}

#[test]
fn an_answer_for_another_site_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut dev = device();
    let code = core.codes().issue(0).unwrap();
    let (id, mut options) = core.enrol_start(&code, "", 0).unwrap();
    // A page elsewhere relaying our options under its own RP ID.
    options.public_key.rp.id = "elsewhere.example".into();
    let answer = dev
        .do_registration(Url::parse("https://elsewhere.example").unwrap(), options)
        .unwrap();
    assert!(matches!(
        core.enrol_finish(&id, &answer, 1),
        Err(Error::Rejected(_))
    ));
    assert!(core.passkeys().is_empty());
}

#[test]
fn sessions_end_idle_or_at_the_cap_and_on_sign_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut dev = device();
    enrol(&mut core, &mut dev, "", 0).unwrap();

    let idle = login(&mut core, &mut dev, 0).unwrap();
    assert!(core.session(&idle, HOUR - 1).unwrap().is_some());
    let gone = login(&mut core, &mut dev, 0).unwrap();
    assert!(core.session(&gone, HOUR).unwrap().is_none());

    // Used every half hour, it lasts the day and no longer, here or after
    // a restart.
    let mut t = 0;
    while t + 1800 < 24 * HOUR {
        t += 1800;
        assert!(core.session(&idle, t).unwrap().is_some(), "at {t}");
    }
    let mut again = Core::open(&config(dir.path())).unwrap();
    assert!(again.session(&idle, t + 1).unwrap().is_some());
    assert!(again.session(&idle, 24 * HOUR).unwrap().is_none());

    let out = login(&mut again, &mut dev, 100).unwrap();
    again.logout(&out).unwrap();
    assert!(again.session(&out, 101).unwrap().is_none());
    assert!(again.session("not a token", 101).unwrap().is_none());
}

#[test]
fn ceremonies_in_flight_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut dev = device();
    enrol(&mut core, &mut dev, "", 0).unwrap();
    let (first, options) = core.login_start(1).unwrap();
    for _ in 0..MAX_PENDING {
        core.login_start(2).unwrap();
    }
    assert_eq!(core.pending.len(), MAX_PENDING);
    let answer = dev.do_authentication(origin(), options).unwrap();
    assert!(matches!(
        core.login_finish(&first, &answer, 3),
        Err(Error::NoCeremony)
    ));
}

#[test]
fn names_are_tidied() {
    let dir = tempfile::tempdir().unwrap();
    let core = Core::open(&config(dir.path())).unwrap();
    assert_eq!(core.name(" my\n  phone\u{7} "), "my phone");
    assert_eq!(core.name(&"x".repeat(100)).len(), MAX_NAME);
    assert_eq!(core.name(""), "Passkey 1");
}

/// Saving the state fails until what's returned is dropped: a directory
/// where its new file goes, which nothing (root included) can write over.
struct Jammed(std::path::PathBuf);

fn jam(dir: &std::path::Path) -> Jammed {
    let tmp = dir.join("state.tmp");
    std::fs::create_dir(&tmp).unwrap();
    std::fs::write(tmp.join("x"), "").unwrap();
    Jammed(tmp)
}

impl Drop for Jammed {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn passkeys_and_sessions_are_the_configured_owners() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut dev = device();
    let token = enrol(&mut core, &mut dev, "Phone", 0).unwrap();

    // The same account under a new login keeps them.
    let mut renamed = config(dir.path());
    renamed.owner.login = "sam-renamed".into();
    let mut same = Core::open(&renamed).unwrap();
    assert_eq!(same.passkeys().len(), 1);
    assert!(same.session(&token, 1).unwrap().is_some());

    // Another account gets none of them, and no pending code either.
    let code = core.codes().issue(1).unwrap();
    let mut other = config(dir.path());
    other.owner.id = 999;
    let mut theirs = Core::open(&other).unwrap();
    assert!(theirs.passkeys().is_empty());
    assert!(theirs.session(&token, 2).unwrap().is_none());
    assert!(matches!(
        theirs.enrol_start(&code, "", 2),
        Err(Error::NoCode)
    ));

    // And they're gone for good: the first owner back finds none.
    let mut back = Core::open(&config(dir.path())).unwrap();
    assert!(back.passkeys().is_empty());
    assert!(back.session(&token, 3).unwrap().is_none());
}

#[test]
fn a_sign_out_that_fails_to_save_signs_nothing_out_and_can_be_retried() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let token = enrol(&mut core, &mut device(), "", 0).unwrap();
    {
        let _jammed = jam(dir.path());
        assert!(matches!(core.logout(&token), Err(Error::Io(_))));
        // Still signed in, here and on disk: the failure says so.
        assert!(core.session(&token, 1).unwrap().is_some());
    }
    core.logout(&token).unwrap();
    assert!(core.session(&token, 2).unwrap().is_none());
    let mut again = Core::open(&config(dir.path())).unwrap();
    assert!(again.session(&token, 3).unwrap().is_none());
}

#[test]
fn a_sign_in_that_fails_to_save_begins_no_session() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut dev = device();
    enrol(&mut core, &mut dev, "", 0).unwrap();
    let (id, options) = core.login_start(10).unwrap();
    let answer = dev.do_authentication(origin(), options).unwrap();
    let jammed = jam(dir.path());
    assert!(matches!(
        core.login_finish(&id, &answer, 11),
        Err(Error::Io(_))
    ));
    drop(jammed);
    // Nothing changed, not even the passkey's last use.
    assert_eq!(core.passkeys()[0].used, Some(0));
    login(&mut core, &mut dev, 12).unwrap();
    assert_eq!(core.passkeys()[0].used, Some(12));
}

#[test]
fn an_enrolment_that_fails_to_save_keeps_its_code() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let mut dev = device();
    let code = core.codes().issue(0).unwrap();
    let (id, options) = core.enrol_start(&code, "Phone", 0).unwrap();
    let answer = dev.do_registration(origin(), options).unwrap();
    let jammed = jam(dir.path());
    assert!(matches!(
        core.enrol_finish(&id, &answer, 1),
        Err(Error::Io(_))
    ));
    drop(jammed);
    assert!(core.passkeys().is_empty());
    // The code is still good for another go.
    let (id, options) = core.enrol_start(&code, "Phone", 2).unwrap();
    let answer = dev.do_registration(origin(), options).unwrap();
    core.enrol_finish(&id, &answer, 3).unwrap();
    assert_eq!(core.passkeys().len(), 1);
}

#[test]
fn a_session_used_often_outlives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let token = enrol(&mut core, &mut device(), "", 0).unwrap();
    // Used every half minute for two hours: more than the idle timeout.
    let mut t = 0;
    while t < 2 * HOUR {
        t += 30;
        assert!(core.session(&token, t).unwrap().is_some(), "at {t}");
    }
    // A restart forgets what wasn't saved, and the session lives on.
    let mut again = Core::open(&config(dir.path())).unwrap();
    assert!(again.session(&token, t + HOUR - 60).unwrap().is_some());
}

#[test]
fn a_use_that_fails_to_save_is_saved_on_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let token = enrol(&mut core, &mut device(), "", 0).unwrap();
    let jammed = jam(dir.path());
    assert!(matches!(core.session(&token, 600), Err(Error::Io(_))));
    drop(jammed);
    assert!(core.session(&token, 601).unwrap().is_some());
    let mut again = Core::open(&config(dir.path())).unwrap();
    // Saved at 601, so it lasts an hour from then.
    assert!(again.session(&token, 601 + HOUR - 1).unwrap().is_some());
}

#[test]
fn a_code_stays_spent_if_its_file_outlives_the_enrolment() {
    let dir = tempfile::tempdir().unwrap();
    let mut core = Core::open(&config(dir.path())).unwrap();
    let code = core.codes().issue(0).unwrap();
    let file = dir.path().join("enroll.json");
    let pending = std::fs::read(&file).unwrap();
    let (id, options) = core.enrol_start(&code, "Laptop", 1).unwrap();
    let answer = device().do_registration(origin(), options).unwrap();
    core.enrol_finish(&id, &answer, 2).unwrap();
    // A crash (or a failed removal) after the passkey was saved and before
    // the code's file went: the file is still there.
    std::fs::write(&file, &pending).unwrap();
    assert!(matches!(
        core.enrol_start(&code, "Phone", 3),
        Err(Error::NoCode)
    ));
    let mut again = Core::open(&config(dir.path())).unwrap();
    assert!(matches!(
        again.enrol_start(&code, "Phone", 4),
        Err(Error::NoCode)
    ));
    assert_eq!(again.passkeys().len(), 1);
}

#[test]
fn a_new_session_never_sweeps_one_in_use_but_not_saved() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.idle_timeout = 30;
    let mut core = Core::open(&cfg).unwrap();
    let mut dev = device();
    let a = enrol(&mut core, &mut dev, "", 0).unwrap();
    // Used every 10 s: alive, and never a grain past its save.
    for t in [10, 20, 30, 40] {
        assert!(core.session(&a, t).unwrap().is_some(), "at {t}");
    }
    // Another sign-in sweeps ended sessions: not this one.
    login(&mut core, &mut dev, 45).unwrap();
    assert!(core.session(&a, 50).unwrap().is_some());
    // And the uses it counted were saved with the new session.
    let mut again = Core::open(&cfg).unwrap();
    assert!(again.session(&a, 69).unwrap().is_some());
}
