//! The kit's negative checks (Conary's `docs/m6-plugins.md`, B2), each
//! against what it's for.
use crate::plugin::Plugin;
use crate::publish;
use crate::records::{End, Record, Records};
use crate::serve::{Controller, Server, Work};
use crate::sign::{self, Signable, Signer};
use burrow_github::fake::{Fake, ok};
use conary_core::op::{Outcome, Repo};
use conary_core::plugin::{
    Answer, Capabilities, Identity, Kind, Next, Request, Response, Sandbox, Task, fixtures,
};
use conary_github::attest::service::Producers;
use conary_github::attest::{Subject, Trust, attested};
use conary_github::flow::{self, Recipe};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const SECRET: [u8; 32] = [9; 32];
const REPO: &str = "acme/widget";
const HEAD: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";

fn me() -> Controller {
    Controller(rustix::process::getuid().as_raw())
}

fn plugin(socket: &Path) -> Plugin {
    Plugin {
        describe: Answer::Describe {
            versions: vec![conary_core::plugin::CONTRACT.into()],
            kinds: vec![Kind::Review],
            v2_kinds: vec![],
            v4_kinds: vec![],
            models: vec![],
            capabilities: Some(Capabilities {
                sandbox: Sandbox::None,
                network: false,
                devices: vec![],
                mcp: false,
            }),
            meters: vec![],
        },
        by: Identity {
            engine: "kit-test".into(),
            session: None,
        },
        fixture: |task| {
            matches!(task, Task::Review { .. }).then(|| Answer::Review { findings: vec![] })
        },
        socket: socket.into(),
    }
}

/// A job request for `repo`: the kind the tests' producers answer.
fn job(repo: &str) -> Value {
    json!({"contract": conary_core::plugin::CONTRACT, "kind": "job", "repo": repo,
        "workdir": "/", "prompt": "do it"})
}

fn answer(summary: String) -> Response {
    Response {
        contract: conary_core::plugin::CONTRACT.into(),
        by: Identity {
            engine: "kit-test".into(),
            session: None,
        },
        execution: None,
        answer: Answer::Job {
            summary,
            next: Next::Done,
        },
    }
}

/// What the plugin writes for `request`, or why it fails.
fn ask(plugin: &Plugin, request: &Value) -> Result<Value, String> {
    let mut out = Vec::new();
    plugin.answer(&mut request.to_string().as_bytes(), &mut out)?;
    Ok(serde_json::from_slice(&out).unwrap())
}

#[test]
fn describe_and_the_fixtures_are_answered_with_no_producer() {
    let dir = tempfile::tempdir().unwrap();
    let p = plugin(&dir.path().join("none.sock"));
    let described = ask(&p, &serde_json::to_value(Request::describe()).unwrap()).unwrap();
    assert_eq!(described["kind"], "describe");
    let review = fixtures()
        .into_iter()
        .find(|f| matches!(f.task, Task::Review { .. }))
        .unwrap();
    let request = serde_json::to_value(Request::work(review.contract, review.task)).unwrap();
    assert_eq!(ask(&p, &request).unwrap()["kind"], "review");
    // Real work needs the producer, and with none there it fails.
    assert!(ask(&p, &job(REPO)).unwrap_err().contains("couldn't reach"));
}

#[test]
fn work_goes_to_the_producer_and_its_error_fails_the_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("p.sock");
    let server = Server::bind(&socket, me()).unwrap();
    let p = plugin(&socket);
    let t = thread::spawn(move || {
        server
            .once(&|_: Request, _: &Work| Ok(answer("done".into())))
            .unwrap();
        server
            .once(&|_: Request, _: &Work| Err("the engine broke".into()))
            .unwrap();
    });
    assert_eq!(ask(&p, &job(REPO)).unwrap()["summary"], "done");
    assert!(
        ask(&p, &job(REPO))
            .unwrap_err()
            .contains("the engine broke")
    );
    t.join().unwrap();
}

#[test]
fn only_the_controller_may_ask() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("p.sock");
    let other = Controller(me().0 + 1);
    let server = Server::bind(&socket, other).unwrap();
    let asked = Arc::new(AtomicU32::new(0));
    let counted = asked.clone();
    let t = thread::spawn(move || {
        server
            .once(&move |_: Request, _: &Work| {
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(answer("done".into()))
            })
            .unwrap();
    });
    let why = ask(&plugin(&socket), &job(REPO)).unwrap_err();
    t.join().unwrap();
    assert!(why.contains("only the flow's controller user"), "{why}");
    assert_eq!(asked.load(Ordering::SeqCst), 0, "no work started");
}

/// A fake GitHub that starts check run 7 on the proof's head and takes
/// any number of changes to it.
fn github() -> Fake {
    let mut fake = Fake::default();
    fake.on(
        "POST",
        "repos/acme/widget/check-runs",
        vec![ok(201, json!({"id": 7}), &[])],
    )
    .on(
        "PATCH",
        "repos/acme/widget/check-runs/7",
        (0..8).map(|_| ok(200, json!({}), &[])).collect(),
    );
    fake
}

/// The conclusions `api` was asked to complete runs with, in order.
fn conclusions(api: &Fake) -> Vec<String> {
    api.asked("PATCH")
        .iter()
        .map(|a| {
            a.2.as_ref().unwrap()["conclusion"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

#[test]
fn a_panicking_producer_fails_its_plugin_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("p.sock");
    let server = Server::bind(&socket, me()).unwrap();
    let serving = thread::spawn(move || {
        let _ = server.once(&|_: Request, _: &Work| -> Result<Response, String> {
            panic!("the producer broke")
        });
    });
    let p = plugin(&socket);
    let (tx, rx) = std::sync::mpsc::channel();
    thread::spawn(move || tx.send(ask(&p, &job(REPO))).unwrap());
    let got = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the plugin isn't left waiting");
    assert!(got.unwrap_err().contains("without answering"));
    assert!(serving.join().is_err());
}

#[test]
fn the_socket_lets_any_user_connect_for_the_peer_check_to_refuse() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("p.sock");
    let _server = Server::bind(&socket, me()).unwrap();
    let mode = std::fs::metadata(&socket).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o666);
}

#[test]
fn a_closed_connection_stops_the_work_and_publishes_no_success() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("p.sock");
    let pid = dir.path().join("pid");
    let server = Server::bind(&socket, me()).unwrap();
    let records = Arc::new(Records::open(&dir.path().join("records")).unwrap());
    let github = Arc::new(Mutex::new(github()));
    let (gh, written, kept) = (github.clone(), pid.clone(), records.clone());
    let t = thread::spawn(move || {
        let handler = move |_: Request, work: &Work| {
            let mut api = gh.lock().unwrap();
            let mut record = kept
                .begin(proof(true).target())
                .map_err(|e| e.to_string())?;
            publish::start(&mut *api, &kept, &mut record)?;
            let ran = work.run(
                Command::new("/bin/sh").args([
                    "-c",
                    &format!("echo $$ > {}; exec sleep 30", written.display()),
                ]),
                None,
                Instant::now() + Duration::from_secs(60),
                1024,
            );
            // Even with a result kept, an abandoned record publishes nothing.
            kept.keep(&mut record, proof(true))?;
            let signer = Signer::for_tests("proofs", SECRET);
            let completed = publish::complete(&mut *api, &kept, &signer, work, &mut record);
            assert!(completed.is_err(), "an abandoned run completes as nothing");
            assert_eq!(record.end(), Some(End::Withdrawn));
            ran.map(|_| answer("ran".into()))
        };
        let started = Instant::now();
        server.once(&handler).unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the work was stopped"
        );
    });
    let mut stream = UnixStream::connect(&socket).unwrap();
    stream
        .write_all(format!("{}\n", job(REPO)).as_bytes())
        .unwrap();
    while !pid.exists() || std::fs::read_to_string(&pid).unwrap().trim().is_empty() {
        thread::sleep(Duration::from_millis(10));
    }
    let child = std::fs::read_to_string(&pid).unwrap().trim().to_owned();
    drop(stream);
    t.join().unwrap();
    assert!(
        !Path::new(&format!("/proc/{child}")).exists(),
        "its process is gone"
    );
    // And a restart after it publishes nothing either.
    let signer = Signer::for_tests("proofs", SECRET);
    let mut api = github.lock().unwrap();
    assert_eq!(
        publish::reconcile::<sign::Proof>(&mut *api, &records, &signer),
        Ok(0)
    );
    assert_eq!(conclusions(&api), ["cancelled"]);
}

#[test]
fn startup_publishes_only_what_was_committed_and_withdraws_the_rest() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::open(&dir.path().join("records")).unwrap();
    let signer = Signer::for_tests("kit", SECRET);
    let mut api = github();
    // A crash before the run's id was kept: found by its external id.
    let lost: Record<sign::Proof> = records.begin(proof(true).target()).unwrap();
    api.on(
        "GET",
        &format!(
            "repos/acme/widget/commits/{HEAD}/check-runs?check_name=proof&filter=all&per_page=100"
        ),
        vec![ok(
            200,
            json!({"check_runs": [{"id": 9, "name": "proof", "external_id": lost.id()}]}),
            &[],
        )],
    )
    .on(
        "PATCH",
        "repos/acme/widget/check-runs/9",
        vec![ok(200, json!({}), &[])],
    );
    // A run whose result was kept but not committed: a restart signs
    // nothing for it.
    let mut kept: Record<sign::Proof> = records.begin(proof(true).target()).unwrap();
    publish::start(&mut api, &records, &mut kept).unwrap();
    records.keep(&mut kept, proof(true)).unwrap();
    // A committed one: published from its record, the work not run again.
    let mut committed: Record<sign::Proof> = records.begin(proof(true).target()).unwrap();
    records.keep(&mut committed, proof(true)).unwrap();
    records.commit(&mut committed).unwrap();
    // One already withdrawn: left be.
    let mut ended: Record<sign::Proof> = records.begin(proof(true).target()).unwrap();
    publish::withdraw(&mut api, &records, &mut ended).unwrap();

    assert_eq!(
        publish::reconcile::<sign::Proof>(&mut api, &records, &signer),
        Ok(3)
    );
    let mut patched: Vec<(String, String)> = api
        .asked("PATCH")
        .iter()
        .map(|a| {
            let body = a.2.as_ref().unwrap();
            (
                a.1.rsplit('/').next().unwrap().to_owned(),
                body["conclusion"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    patched.sort();
    // Run 7 is the kept and the committed records' (the fake gives every
    // new run that id): one cancelled, one published.
    assert_eq!(
        patched,
        [
            ("7".into(), "cancelled".into()),
            ("7".into(), "success".into()),
            ("9".into(), "cancelled".into()),
        ]
    );
    let published = api
        .asked("PATCH")
        .iter()
        .find(|a| a.2.as_ref().unwrap()["conclusion"] == "success")
        .map(|a| a.2.clone().unwrap())
        .unwrap();
    let recipe = flow::recipe_digest(&proof(true).recipe);
    let text = published["output"]["text"].as_str().unwrap();
    assert!(verify(text, "success", "proof", TREE, Some(&recipe)).is_ok());
    assert!(records.unfinished::<sign::Proof>().unwrap().is_empty());
    assert_eq!(
        publish::reconcile::<sign::Proof>(&mut api, &records, &signer),
        Ok(0)
    );
}

#[test]
fn a_committed_record_is_published_even_once_its_work_is_abandoned() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::open(&dir.path().join("records")).unwrap();
    let signer = Signer::for_tests("kit", SECRET);
    let mut api = github();
    let mut record: Record<sign::Judgment> = records.begin(judgment().target()).unwrap();
    publish::start(&mut api, &records, &mut record).unwrap();
    records.keep(&mut record, judgment()).unwrap();
    // Its sealed comment went out: it's never withdrawn.
    records.commit(&mut record).unwrap();
    publish::finish(&mut api, &records, &signer, true, &mut record).unwrap();
    assert_eq!(record.end(), Some(End::Published));
    assert_eq!(conclusions(&api), ["success"]);
}

#[test]
fn a_result_published_on_its_own_must_be_committed_first() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::open(&dir.path().join("records")).unwrap();
    let signer = Signer::for_tests("kit", SECRET);
    let mut api = github();
    let mut record: Record<sign::Judgment> = records.begin(judgment().target()).unwrap();
    records.keep(&mut record, judgment()).unwrap();
    // Uncommitted, it could still be withdrawn: nothing goes out.
    assert!(publish::publish(&mut api, &records, &signer, &mut record).is_err());
    assert!(api.asked("POST").is_empty());
    assert_eq!(record.end(), None);
    records.commit(&mut record).unwrap();
    let signed = publish::publish(&mut api, &records, &signer, &mut record).unwrap();
    assert_eq!(signed.check(), "conary/judgment");
    assert_eq!(record.end(), Some(End::Published));
    assert_eq!(conclusions(&api), ["success"]);
    let text = api.asked("PATCH")[0].2.as_ref().unwrap()["output"]["text"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(verify(&text, "success", "conary/judgment", TREE, None).is_ok());
}

#[test]
fn a_published_result_survives_a_crash_before_its_end_was_kept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("records");
    let records = Records::open(&path).unwrap();
    let signer = Signer::for_tests("kit", SECRET);
    let mut api = github();
    let mut record: Record<sign::Proof> = records.begin(proof(true).target()).unwrap();
    publish::start(&mut api, &records, &mut record).unwrap();
    records.keep(&mut record, proof(true)).unwrap();
    let before = std::fs::read(path.join(format!("{}.json", record.id()))).unwrap();
    publish::finish(&mut api, &records, &signer, false, &mut record).unwrap();
    // The crash: GitHub has the result, and the record's end was lost.
    let file = path.join(format!("{}.json", record.id()));
    let mut kept: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_ne!(std::fs::read(&file).unwrap(), before);
    kept["end"] = Value::Null;
    std::fs::write(&file, kept.to_string()).unwrap();
    assert_eq!(
        publish::reconcile::<sign::Proof>(&mut api, &records, &signer),
        Ok(1)
    );
    assert_eq!(
        conclusions(&api),
        ["success", "success"],
        "published again, never withdrawn"
    );
    // Nor can anything withdraw a committed record by hand.
    let mut committed: Record<sign::Judgment> = records.begin(judgment().target()).unwrap();
    publish::start(&mut api, &records, &mut committed).unwrap();
    records.keep(&mut committed, judgment()).unwrap();
    records.commit(&mut committed).unwrap();
    assert!(publish::withdraw(&mut api, &records, &mut committed).is_err());
    assert_eq!(committed.end(), None);
    assert_eq!(conclusions(&api), ["success", "success"]);
}

#[test]
fn every_request_runs_its_handler_and_answers_only_with_what_it_returned() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("p.sock");
    let server = Server::bind(&socket, me()).unwrap();
    let runs = Arc::new(AtomicU32::new(0));
    let counted = runs.clone();
    let t = thread::spawn(move || {
        let handler = move |_: Request, _: &Work| {
            let n = counted.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(answer(format!("look {n}")))
        };
        server.once(&handler).unwrap();
        server.once(&handler).unwrap();
    });
    let p = plugin(&socket);
    let first = ask(&p, &job(REPO)).unwrap();
    let second = ask(&p, &job(REPO)).unwrap();
    t.join().unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2, "the same request ran twice");
    assert_eq!(first["summary"], "look 1");
    assert_eq!(second["summary"], "look 2");
}

#[test]
fn records_survive_whole_and_hold_one_result_for_their_own_target() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::open(&dir.path().join("records")).unwrap();
    let mut a: Record<sign::Proof> = records.begin(proof(true).target()).unwrap();
    let b: Record<sign::Proof> = records.begin(proof(true).target()).unwrap();
    assert_ne!(a.id(), b.id());
    // Another check's result, or a second one, isn't this record's.
    let mut other = proof(true);
    other.check = "other".into();
    assert!(records.keep(&mut a, other).is_err());
    assert!(records.commit(&mut a).is_err(), "nothing to commit yet");
    records.keep(&mut a, proof(true)).unwrap();
    assert!(records.keep(&mut a, proof(false)).is_err());
    records.commit(&mut a).unwrap();
    let left = records.unfinished::<sign::Proof>().unwrap();
    assert_eq!(left.len(), 2);
    let read = left.iter().find(|r| r.id() == a.id()).unwrap();
    assert_eq!(read, &a, "read back as kept");
    records.ended(&mut a, End::Published).unwrap();
    let left = records.unfinished::<sign::Proof>().unwrap();
    assert_eq!(left, vec![b]);
    // Only whole files: no temporary one is left beside them.
    let kept = dir.path().join("records");
    assert_eq!(std::fs::read_dir(&kept).unwrap().count(), 2);
    std::fs::write(kept.join("broken.json"), "{").unwrap();
    assert!(
        records.unfinished::<sign::Proof>().is_err(),
        "never skipped"
    );
    // A directory others can read isn't the producer's alone.
    let shared = dir.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    burrow_test::permissions(&shared, 0o755);
    assert!(Records::open(&shared).is_err());
}

fn proof(passed: bool) -> sign::Proof {
    sign::Proof {
        repo: REPO.into(),
        head: HEAD.into(),
        tree: TREE.into(),
        check: "proof".into(),
        recipe: Recipe::Build(".#checks.x86_64-linux.gate".into()),
        passed,
        log: flow::receipt_digest(b"build output"),
    }
}

fn judgment() -> sign::Judgment {
    sign::Judgment {
        repo: REPO.into(),
        pr: 5,
        revision: flow::Revision {
            head: HEAD.into(),
            base: "3333333333333333333333333333333333333333".into(),
            description: "Fix it".into(),
        },
        tree: TREE.into(),
        generation: 4,
        verdict: Some(conary_core::plugin::Judgment::Pass),
        provenance: None,
        basis: None,
        reasons: "Sound.".into(),
    }
}

/// What Conary's verifier says of `text` published as check run `name` on
/// `tree`, by producer `producer` trusted for `checks`.
fn verify(
    text: &str,
    conclusion: &str,
    name: &str,
    tree: &str,
    definition: Option<&str>,
) -> Result<(Outcome, Option<String>), String> {
    let public = ed25519_dalek::SigningKey::from_bytes(&SECRET).verifying_key();
    use base64::Engine as _;
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.as_bytes());
    let producers = Producers::parse(
        json!([{"name": "kit", "key": key,
            "checks": ["conary/judgment", "proof"]}])
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let trust = Trust {
        signer: "Acme/ci/.github/workflows/gate.yml".parse().unwrap(),
        fresh: None,
        producers,
    };
    let run: conary_github::api::CheckRun = serde_json::from_value(json!({
        "id": 1, "name": name, "status": "completed", "conclusion": conclusion,
        "html_url": null, "started_at": null, "check_suite": null,
        "output": {"text": text}}))
    .unwrap();
    // A service statement is checked from the run alone: GitHub is never
    // asked, so this one answers nothing.
    let github =
        conary_github::Api::new("http://127.0.0.1:1", conary_github::Tokens::given("unused"));
    let repo: Repo = REPO.parse().unwrap();
    attested(
        &github,
        &github,
        &repo,
        Subject::Commit {
            commit: HEAD,
            tree,
            definition,
        },
        &run,
        &trust,
    )
    .unwrap()
    .map(|a| (a.result, a.log))
}

/// `data`, kept in a new record in `dir`.
fn kept<T: Signable>(dir: &Path, data: T) -> Record<T> {
    let records = Records::open(&dir.join("records")).unwrap();
    let mut record = records.begin(data.target()).unwrap();
    records.keep(&mut record, data).unwrap();
    record
}

#[test]
fn signed_results_verify_through_conarys_verifier_and_nothing_else_does() {
    let dir = tempfile::tempdir().unwrap();
    let signer = Signer::for_tests("kit", SECRET);
    let sign = |record| signer.signed(&kept(dir.path(), record)).unwrap();
    let j = signer.signed(&kept(dir.path(), judgment())).unwrap();
    let judged = judgment();
    let receipt = flow::receipt_digest(&flow::judgment_receipt_by(
        &judged.revision,
        4,
        judged.verdict,
        None,
        None,
    ));
    assert_eq!(
        verify(j.text(), j.conclusion(), "conary/judgment", TREE, None),
        Ok((Outcome::Pass, Some(receipt)))
    );
    let recipe = flow::recipe_digest(&proof(true).recipe);
    let p = sign(proof(true));
    let checked = verify(p.text(), p.conclusion(), "proof", TREE, Some(&recipe));
    assert_eq!(checked.map(|c| c.0), Ok(Outcome::Pass));
    // A failed proof is signed as one, on a failed run: Conary counts it as
    // no pass, whatever run carries it.
    let failed = sign(proof(false));
    assert_eq!(failed.conclusion(), "failure");
    for conclusion in ["failure", "success"] {
        assert!(verify(failed.text(), conclusion, "proof", TREE, Some(&recipe)).is_err());
    }

    // The same statement, altered: another check, another tree, another
    // log, or another recipe, never counts.
    let p = sign(proof(true));
    let other = "4444444444444444444444444444444444444444";
    assert!(verify(p.text(), "success", "other", TREE, Some(&recipe)).is_err());
    assert!(verify(p.text(), "success", "proof", other, Some(&recipe)).is_err());
    let log = flow::receipt_digest(b"build output");
    let edited = p
        .text()
        .replace(&log, &flow::receipt_digest(b"something else"));
    assert_ne!(edited, p.text());
    assert!(verify(&edited, "success", "proof", TREE, Some(&recipe)).is_err());
    let changed = flow::recipe_digest(&Recipe::Build(".#checks.x86_64-linux.other".into()));
    assert!(verify(p.text(), "success", "proof", TREE, Some(&changed)).is_err());
    // A pass can't be published as a failure's run, nor the reverse.
    assert!(verify(p.text(), "failure", "proof", TREE, Some(&recipe)).is_err());
}

#[test]
fn a_key_comes_only_from_its_credential() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("proof-key");
    std::fs::write(&key, SECRET).unwrap();
    burrow_test::permissions(&key, 0o400);
    let credentials = dir.path().as_os_str().to_owned();
    let with = |vars: Vec<(&'static str, std::ffi::OsString)>| {
        move |name: &str| {
            vars.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.clone())
        }
    };
    // The environment holding a key, or a path to one, is no credential.
    let offered = with(vec![
        (
            "PRODUCER_KEY",
            "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQk".into(),
        ),
        ("KEY_FILE", key.as_os_str().to_owned()),
    ]);
    assert!(Signer::load_with(&offered, "proofs", "proof-key").is_err());
    // A path as the credential's name is refused, the right file or not.
    let given = with(vec![("CREDENTIALS_DIRECTORY", credentials.clone())]);
    for name in [
        key.to_str().unwrap(),
        "../proof-key",
        "..",
        "",
        "./proof-key",
    ] {
        assert!(Signer::load_with(&given, "proofs", name).is_err(), "{name}");
    }
    // A key others could read isn't the producer's alone.
    burrow_test::permissions(&key, 0o644);
    assert!(Signer::load_with(&given, "proofs", "proof-key").is_err());
    burrow_test::permissions(&key, 0o400);
    let signer = Signer::load_with(&given, "kit", "proof-key").unwrap();
    let p = signer.signed(&kept(dir.path(), proof(true))).unwrap();
    let recipe = flow::recipe_digest(&proof(true).recipe);
    assert!(verify(p.text(), "success", "proof", TREE, Some(&recipe)).is_ok());
}

/// Each source file's public functions, each signature as one line.
fn public_signatures() -> Vec<(String, String)> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        let file = path.file_name().unwrap().to_str().unwrap().to_owned();
        if file == "tests.rs" {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines = BufReader::new(text.as_bytes()).lines().map(Result::unwrap);
        while let Some(line) = lines.next() {
            if !line.trim_start().starts_with("pub fn") {
                continue;
            }
            let mut signature = line.clone();
            while !signature.contains('{') {
                signature.push_str(&lines.next().unwrap());
            }
            out.push((file.clone(), signature));
        }
    }
    out
}

/// The names of `file`'s public functions.
fn public_in(signatures: &[(String, String)], file: &str) -> Vec<String> {
    let mut names: Vec<String> = signatures
        .iter()
        .filter(|(f, _)| f == file)
        .map(|(_, s)| {
            let name = s.trim_start().trim_start_matches("pub fn ");
            name[..name.find(['(', '<']).unwrap()].to_owned()
        })
        .collect();
    names.sort();
    names
}

#[test]
fn nothing_public_signs_a_statement_a_caller_hands_it_or_reads_a_record_back() {
    let signatures = public_signatures();
    assert!(signatures.len() > 10, "{signatures:?}");
    for (_, s) in &signatures {
        assert!(!s.contains("Statement"), "{s}");
        assert!(!s.contains("SigningKey"), "{s}");
    }
    // Signing happens only as a record is published: the signer's public
    // calls load its key and read a signed result, and nothing else, but
    // for a test's signer, which no build that runs has.
    assert_eq!(
        public_in(&signatures, "sign.rs"),
        ["check", "conclusion", "for_tests", "load", "text"]
    );
    let sign =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sign.rs")).unwrap();
    assert!(
        sign.contains(
            "    #[cfg(any(test, feature = \"test-key\"))]\n    pub fn for_tests(producer: &str, secret: [u8; 32]) -> Self {"
        ),
        "a test's signer is only in a test's build"
    );
    assert_eq!(sign.matches("pub fn for_tests").count(), 1);
    assert!(
        sign.contains("\nmod sealed {"),
        "how a result signs stays private"
    );
    // A signed result is made only by the signer: its fields are its own.
    let signed = sign.split("pub struct Signed {").nth(1).unwrap();
    let fields = &signed[..signed.find('}').unwrap()];
    assert!(!fields.contains("pub "), "{fields}");
    // Nothing public reads a record back, so none can become an answer: a
    // record is made, kept and committed, and only the crate's startup
    // reconciliation reads the directory.
    assert_eq!(
        public_in(&signatures, "records.rs"),
        [
            "begin",
            "commit",
            "committed",
            "data",
            "end",
            "id",
            "keep",
            "open",
            "target"
        ]
    );
    let records =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/records.rs"))
            .unwrap();
    let record = records.split("pub struct Record<T> {").nth(1).unwrap();
    let fields = &record[..record.find('}').unwrap()];
    assert!(!fields.contains("pub "), "{fields}");
    assert!(
        !records.contains("Deserialize)]\npub struct Record"),
        "a record can't be made by reading one in"
    );
}

#[test]
fn a_key_is_its_producers_alone_by_its_mode_and_by_its_acl() {
    use crate::sign::others_may_read;
    let acl = |entries: &[(u16, u16, u32)]| {
        let mut out = 2u32.to_le_bytes().to_vec();
        for (tag, perm, id) in entries {
            out.extend(tag.to_le_bytes());
            out.extend(perm.to_le_bytes());
            out.extend(id.to_le_bytes());
        }
        out
    };
    // As systemd hands a service its credential: root's file, 0400, and
    // an ACL entry for the service's user (the mode's group bits are the
    // ACL's mask).
    let systemd = acl(&[
        (1, 4, 0),
        (2, 4, 1000),
        (4, 0, 0),
        (0x10, 4, 0),
        (0x20, 0, 0),
    ]);
    assert_eq!(others_may_read(0o440, 0, Some(&systemd), 1000), Ok(false));
    // Someone else's credential.
    assert_eq!(others_may_read(0o440, 0, Some(&systemd), 1001), Ok(true));
    // The owning group, or a named one, through the mask.
    let group = acl(&[
        (1, 4, 0),
        (2, 4, 1000),
        (4, 4, 0),
        (0x10, 4, 0),
        (0x20, 0, 0),
    ]);
    assert_eq!(others_may_read(0o440, 0, Some(&group), 1000), Ok(true));
    let named = acl(&[
        (1, 4, 0),
        (2, 4, 1000),
        (4, 0, 0),
        (8, 4, 50),
        (0x10, 4, 0),
        (0x20, 0, 0),
    ]);
    assert_eq!(others_may_read(0o440, 0, Some(&named), 1000), Ok(true));
    // A mask that grants nothing leaves only the owner.
    let masked = acl(&[
        (1, 4, 1000),
        (2, 4, 1001),
        (4, 4, 0),
        (0x10, 0, 0),
        (0x20, 0, 0),
    ]);
    assert_eq!(others_may_read(0o400, 1000, Some(&masked), 1000), Ok(false));
    // Others, by the mode, whatever the ACL.
    assert_eq!(others_may_read(0o404, 0, Some(&systemd), 1000), Ok(true));
    // No ACL: the mode alone.
    assert_eq!(others_may_read(0o400, 1000, None, 1000), Ok(false));
    assert_eq!(others_may_read(0o440, 1000, None, 1000), Ok(true));
    assert_eq!(others_may_read(0o400, 1001, None, 1000), Ok(true));
    // An ACL that doesn't read as one is no answer.
    assert!(others_may_read(0o400, 1000, Some(&[1, 0, 0, 0]), 1000).is_err());
    assert!(others_may_read(0o400, 1000, Some(&systemd[..10]), 1000).is_err());
}
