//! The kit's negative checks (Conary's `docs/m6-plugins.md`, B2), each
//! against what it's for.
use crate::plugin::Plugin;
use crate::publish;
use crate::records::Records;
use crate::serve::{Controller, Server, Work};
use crate::sign::{self, Signer};
use burrow_github::fake::{Fake, ok};
use conary_core::op::{Outcome, Repo};
use conary_core::plugin::{Answer, Identity, Kind, Next, Request, Response, Task, fixtures};
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
            capabilities: None,
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

#[test]
fn a_closed_connection_stops_the_work_and_publishes_no_success() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("p.sock");
    let pid = dir.path().join("pid");
    let server = Server::bind(&socket, me()).unwrap();
    let github = Arc::new(Mutex::new(Fake::default()));
    github
        .lock()
        .unwrap()
        .on(
            "POST",
            "repos/acme/widget/check-runs",
            vec![ok(201, json!({"id": 7}), &[])],
        )
        .on(
            "PATCH",
            "repos/acme/widget/check-runs/7",
            vec![ok(200, json!({}), &[])],
        );
    let (gh, written) = (github.clone(), pid.clone());
    let t = thread::spawn(move || {
        let handler = move |_: Request, work: &Work| {
            let mut api = gh.lock().unwrap();
            let run = publish::start(&mut *api, REPO, HEAD, "proof", "r1")?;
            let ran = work.run(
                Command::new("/bin/sh").args([
                    "-c",
                    &format!("echo $$ > {}; exec sleep 30", written.display()),
                ]),
                None,
                Instant::now() + Duration::from_secs(60),
                1024,
            );
            let signed = Signer::for_tests("proofs", SECRET).proof(&proof(true));
            let completed = publish::complete(&mut *api, work, &run, &signed);
            assert!(completed.is_err(), "an abandoned run completes as nothing");
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
    let api = github.lock().unwrap();
    let patched: Vec<String> = api
        .asked("PATCH")
        .iter()
        .map(|a| a.2.as_ref().unwrap()["conclusion"].to_string())
        .collect();
    assert_eq!(patched, ["\"cancelled\""]);
}

#[test]
fn every_request_is_new_work_and_never_a_recorded_answer() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("p.sock");
    let server = Server::bind(&socket, me()).unwrap();
    let records = Records::open(&dir.path().join("records")).unwrap();
    let runs = Arc::new(AtomicU32::new(0));
    let counted = runs.clone();
    let t = thread::spawn(move || {
        let handler = move |_: Request, _: &Work| {
            let n = counted.fetch_add(1, Ordering::SeqCst) + 1;
            let record = records
                .keep(format!("look {n}"))
                .map_err(|e| e.to_string())?;
            Ok(answer(format!("{} as {}", record.data, record.id)))
        };
        server.once(&handler).unwrap();
        server.once(&handler).unwrap();
    });
    let p = plugin(&socket);
    let first = ask(&p, &job(REPO)).unwrap()["summary"].clone();
    let second = ask(&p, &job(REPO)).unwrap()["summary"].clone();
    t.join().unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 2, "the same request ran twice");
    assert!(first.as_str().unwrap().starts_with("look 1 as "));
    assert!(second.as_str().unwrap().starts_with("look 2 as "));
    assert_ne!(first, second);
}

#[test]
fn records_survive_whole_and_only_unpublished_ones_come_back() {
    let dir = tempfile::tempdir().unwrap();
    let records = Records::open(dir.path()).unwrap();
    let mut a = records.keep(json!({"head": HEAD})).unwrap();
    let b = records.keep(json!({"head": TREE})).unwrap();
    assert_ne!(a.id, b.id);
    records.publish(&mut a).unwrap();
    let left: Vec<crate::records::Record<Value>> = records.unpublished().unwrap();
    assert_eq!(left, vec![b]);
    // Only whole files: no temporary one is left beside them.
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    std::fs::write(dir.path().join("broken.json"), "{").unwrap();
    assert!(records.unpublished::<Value>().is_err(), "never skipped");
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

fn review() -> sign::Review {
    sign::Review {
        repo: REPO.into(),
        role: "codex".into(),
        head: HEAD.into(),
        tree: TREE.into(),
        body: "No findings.".into(),
    }
}

fn judgment() -> sign::Judgment {
    sign::Judgment {
        repo: REPO.into(),
        revision: flow::Revision {
            head: HEAD.into(),
            base: "3333333333333333333333333333333333333333".into(),
            description: "Fix it".into(),
        },
        tree: TREE.into(),
        generation: 4,
        verdict: Some(conary_core::plugin::Judgment::Pass),
        provenance: None,
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
            "checks": ["conary/review/*", "conary/judgment", "proof"]}])
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

#[test]
fn signed_results_verify_through_conarys_verifier_and_nothing_else_does() {
    let signer = Signer::for_tests("kit", SECRET);
    let r = signer.review(&review());
    let receipt = flow::receipt_digest(&flow::review_receipt(HEAD, "No findings."));
    assert_eq!(
        verify(r.text(), r.conclusion(), "conary/review/codex", TREE, None),
        Ok((Outcome::Pass, Some(receipt)))
    );
    let j = signer.judgment(&judgment());
    let judged = judgment();
    let receipt = flow::receipt_digest(&flow::judgment_receipt_by(
        &judged.revision,
        4,
        judged.verdict,
        None,
    ));
    assert_eq!(
        verify(j.text(), j.conclusion(), "conary/judgment", TREE, None),
        Ok((Outcome::Pass, Some(receipt)))
    );
    let recipe = flow::recipe_digest(&proof(true).recipe);
    let p = signer.proof(&proof(true));
    let checked = verify(p.text(), p.conclusion(), "proof", TREE, Some(&recipe));
    assert_eq!(checked.map(|c| c.0), Ok(Outcome::Pass));
    // A failed proof is signed as one, on a failed run: Conary counts it as
    // no pass, whatever run carries it.
    let failed = signer.proof(&proof(false));
    assert_eq!(failed.conclusion(), "failure");
    for conclusion in ["failure", "success"] {
        assert!(verify(failed.text(), conclusion, "proof", TREE, Some(&recipe)).is_err());
    }

    // The same statement, altered: another check, another tree, another
    // log, or another recipe, never counts.
    let p = signer.proof(&proof(true));
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
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("proof-key");
    std::fs::write(&key, SECRET).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o400)).unwrap();
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
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(Signer::load_with(&given, "proofs", "proof-key").is_err());
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o400)).unwrap();
    let signer = Signer::load_with(&given, "kit", "proof-key").unwrap();
    let p = signer.proof(&proof(true));
    let recipe = flow::recipe_digest(&proof(true).recipe);
    assert!(verify(p.text(), "success", "proof", TREE, Some(&recipe)).is_ok());
}

/// The crate's public functions, each signature as one line.
fn public_signatures() -> Vec<String> {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap() == "tests.rs" {
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
            out.push(signature);
        }
    }
    out
}

#[test]
fn nothing_public_signs_a_statement_a_caller_hands_it() {
    let signatures = public_signatures();
    assert!(signatures.len() > 10, "{signatures:?}");
    for s in &signatures {
        assert!(!s.contains("Statement"), "{s}");
        assert!(!s.contains("SigningKey"), "{s}");
    }
    // A signed result is made only by the signer: its fields are its own.
    let sign =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sign.rs")).unwrap();
    let signed = sign.split("pub struct Signed {").nth(1).unwrap();
    let fields = &signed[..signed.find('}').unwrap()];
    assert!(!fields.contains("pub "), "{fields}");
}
