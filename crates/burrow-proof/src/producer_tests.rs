//! The proof plugin, its producer and its worker: the producer against a
//! scripted GitHub and scripted units, through the plugin and the kit's
//! socket, as the flow reaches it, on real git repos; the worker's `Run`
//! recipes on a real mirror, with no cgroups.
use crate::fence::Fence;
use crate::plugin::{Config, describe, plugin};
use crate::producer::{GitHub, Producer, lay};
use crate::work::{self, BROKE, FAILED, Mode, PASSED, SPEC, Spec};
use burrow_github::fake::{Fake, ok};
use burrow_github::{Api, Failed, Response as GhResponse};
use burrow_producer::publish;
use burrow_producer::records::{Records, Target};
use burrow_producer::serve::{Controller, Server, Work};
use burrow_producer::sign::{Proof, Signer};
use burrow_producer::units::{Ended, Units, ended, run_id};
use conary_core::op::{Outcome, Repo};
use conary_core::plugin::{Answer, Capabilities, Kind, Model, Offers, Request, Sandbox};
use conary_github::attest::service::Producers;
use conary_github::attest::{Subject, Trust, attested};
use conary_github::flow::{self, Recipe};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const REPO: &str = "acme/widget";
const SECRET: [u8; 32] = [7; 32];
const PRODUCER: &str = "machine-proof";

/// A GitHub every request's handle shares, so a test sees all it was asked.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Fake>>);

impl Api for Shared {
    fn request(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        etag: Option<&str>,
    ) -> Result<GhResponse, Failed> {
        self.0.lock().unwrap().request(method, path, body, etag)
    }
}

/// Units that run nothing: each run's end is scripted, and its log is
/// what the test says it printed.
#[derive(Clone, Default)]
struct Scripted(Arc<Mutex<Script>>);

#[derive(Default)]
struct Gate {
    state: Mutex<(usize, usize, bool)>,
    changed: Condvar,
}

#[derive(Default)]
struct Script {
    runs: PathBuf,
    ends: VecDeque<Ended>,
    printed: String,
    /// Whether a run waits until its work is abandoned.
    hang: bool,
    /// Optional barrier tracking concurrent active/peak unit runs.
    gate: Option<Arc<Gate>>,
    started: Vec<Spec>,
    stopped: Vec<String>,
    loaded: Vec<String>,
}

impl Units for Scripted {
    fn run(&self, id: &str, work: &Work, until: Instant) -> Result<Ended, String> {
        let (hang, gate) = {
            let mut s = self.0.lock().unwrap();
            let spec: Spec =
                serde_json::from_slice(&std::fs::read(s.runs.join(id).join(SPEC)).unwrap())
                    .unwrap();
            s.started.push(spec);
            // The log the producer laid out, written as a worker would.
            let mut log = std::fs::OpenOptions::new()
                .append(true)
                .open(s.runs.join(id).join("log"))
                .unwrap();
            log.write_all(s.printed.as_bytes()).unwrap();
            (s.hang, s.gate.clone())
        };
        if let Some(gate) = gate {
            {
                let mut g = gate.state.lock().unwrap();
                g.0 += 1;
                g.1 = g.1.max(g.0);
                gate.changed.notify_all();
                while !g.2 && !work.abandoned() && Instant::now() < until {
                    let (guard, _) = gate
                        .changed
                        .wait_timeout(g, Duration::from_millis(10))
                        .unwrap();
                    g = guard;
                }
                g.0 -= 1;
                gate.changed.notify_all();
            }
            if work.abandoned() {
                return Err("the request was abandoned".into());
            }
        }
        if hang {
            while !work.abandoned() && Instant::now() < until {
                thread::sleep(Duration::from_millis(10));
            }
            return Err("the request was abandoned".into());
        }
        let mut s = self.0.lock().unwrap();
        Ok(s.ends.pop_front().expect("a scripted end"))
    }

    fn stop(&self, id: &str) -> Result<(), String> {
        self.0.lock().unwrap().stopped.push(id.into());
        Ok(())
    }

    fn loaded(&self) -> Result<Vec<String>, String> {
        Ok(self.0.lock().unwrap().loaded.clone())
    }
}

struct Rig {
    dir: tempfile::TempDir,
    cfg: Config,
    github: Shared,
    units: Scripted,
    producer: Arc<Producer>,
    /// The repo's commits: the first, and another with the same tree.
    head: String,
    twin: String,
    tree: String,
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn config(d: &Path) -> Config {
    serde_json::from_value(json!({
        "socket": d.join("producer.sock"), "controller": "flow",
        "producer": PRODUCER, "credential": "proof-key",
        "flow": d.join("flow"), "gh": "gh", "git": "git",
        "github": format!("file://{}", d.join("github").display()),
        "state": d.join("state"),
        "unit": "autoland-proof-work", "systemctl": "systemctl",
        "timeout": 600, "nix": "/nix/bin", "heavy": null,
        "trees": d.join("trees"),
    }))
    .unwrap()
}

/// The repo's flow settings, with `recipes` its proofs.
fn settings(d: &Path, recipes: Value) {
    std::fs::create_dir_all(d.join("flow").join("acme")).unwrap();
    std::fs::write(
        d.join("flow").join("acme").join("widget.json"),
        json!({"settings": {"proofs": recipes}}).to_string(),
    )
    .unwrap();
}

fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // GitHub's copy of the repo, with two commits of one tree.
    let work = d.join("work");
    std::fs::create_dir_all(&work).unwrap();
    git(&work, &["init", "-q"]);
    std::fs::write(work.join("README"), "widget\n").unwrap();
    git(&work, &["add", "README"]);
    git(&work, &["commit", "-q", "-m", "one"]);
    let head = git(&work, &["rev-parse", "HEAD"]);
    let tree = git(&work, &["rev-parse", "HEAD^{tree}"]);
    let twin = git(&work, &["commit-tree", &tree, "-m", "the same tree again"]);
    git(&work, &["branch", "twin", &twin]);
    let github = d.join("github").join("acme");
    std::fs::create_dir_all(&github).unwrap();
    git(
        d,
        &[
            "clone",
            "-q",
            "--bare",
            work.to_str().unwrap(),
            github.join("widget.git").to_str().unwrap(),
        ],
    );
    git(
        &github.join("widget.git"),
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );
    settings(
        d,
        json!({"agents": {"Build": "checks.x86_64-linux.agents"},
               "gate": {"Run": ["bash", "-c", "make gate"]},
               "think": {"Job": "think hard"}}),
    );
    let cfg = config(d);
    let api = Shared::default();
    {
        let mut fake = api.0.lock().unwrap();
        for commit in [&head, &twin] {
            fake.on(
                "GET",
                &format!("repos/{REPO}/git/commits/{commit}"),
                vec![ok(200, json!({"tree": {"sha": tree}}), &[])],
            );
        }
        fake.on(
            "POST",
            &format!("repos/{REPO}/check-runs"),
            vec![ok(201, json!({"id": 7}), &[])],
        )
        .on(
            "PATCH",
            &format!("repos/{REPO}/check-runs/7"),
            vec![ok(200, json!({}), &[])],
        );
    }
    let units = Scripted::default();
    {
        let mut s = units.0.lock().unwrap();
        s.runs = cfg.state.join("runs");
        s.printed = "built\n".into();
    }
    let shared = api.clone();
    let github: GitHub = Box::new(move || Box::new(shared.clone()));
    let producer = Producer::new(
        cfg.clone(),
        github,
        Box::new(units.clone()),
        Signer::for_tests(PRODUCER, SECRET),
    )
    .unwrap();
    Rig {
        dir,
        cfg,
        github: api,
        units,
        producer: Arc::new(producer),
        head,
        twin,
        tree,
    }
}

impl Rig {
    /// Serve `n` connections in the background.
    fn serve(&self, n: usize) -> thread::JoinHandle<()> {
        let me = Controller(rustix::process::getuid().as_raw());
        let server = Server::bind(&self.cfg.socket, me).unwrap();
        let producer = self.producer.clone();
        thread::spawn(move || {
            let handler = |r: Request, w: &Work| producer.handle(r, w);
            for _ in 0..n {
                server.once(&handler).unwrap();
            }
        })
    }

    fn ask(&self, request: &Value) -> Result<Value, String> {
        let served = self.serve(1);
        let mut out = Vec::new();
        let got = plugin(&self.cfg).answer(&mut request.to_string().as_bytes(), &mut out);
        served.join().unwrap();
        got.map(|()| serde_json::from_slice(&out).unwrap())
    }

    fn script(&self, ends: &[Ended]) {
        self.units.0.lock().unwrap().ends = ends.iter().cloned().collect();
    }

    fn started(&self) -> Vec<Spec> {
        self.units.0.lock().unwrap().started.clone()
    }

    fn stopped(&self) -> Vec<String> {
        self.units.0.lock().unwrap().stopped.clone()
    }

    /// What GitHub was asked to do to check runs, in order: each run
    /// started, and each completion's conclusion and output text.
    fn runs(&self) -> Vec<(String, Option<String>)> {
        let fake = self.github.0.lock().unwrap();
        fake.asked
            .iter()
            .filter(|a| a.0 != "GET")
            .map(|a| {
                let body = a.2.clone().unwrap_or_default();
                match a.0.as_str() {
                    "POST" => (format!("start {}", body["name"].as_str().unwrap()), None),
                    _ => (
                        body["conclusion"].as_str().unwrap_or_default().to_owned(),
                        body["output"]["text"].as_str().map(str::to_owned),
                    ),
                }
            })
            .collect()
    }
}

/// A `kind` request, carrying the describe snapshot a v5 request does.
fn request(kind: &str, head: &str, tree: &str, checks: &[&str]) -> Value {
    json!({"contract": conary_core::plugin::CONTRACT, "kind": kind, "repo": REPO,
        "head": head, "tree": tree, "checks": checks,
        "execution": {"choice": null, "context": {"kind": "fresh"}},
        "negotiated": {"by": {"engine": crate::plugin::ENGINE, "session": null},
            "offers": {"models": [],
                "capabilities": {"sandbox": "brings", "network": true, "devices": [], "mcp": false},
                "meters": []}}})
}

/// What Conary's verifier says of `text`, published as `name`'s check run
/// on `commit` (whose tree is `tree`), defined by `recipe`, by this
/// producer trusted for `name`.
fn verify(
    text: &str,
    conclusion: &str,
    name: &str,
    commit: &str,
    tree: &str,
    recipe: &Recipe,
) -> Result<(Outcome, Option<String>), String> {
    use base64::Engine as _;
    let public = ed25519_dalek::SigningKey::from_bytes(&SECRET).verifying_key();
    let key = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public.as_bytes());
    let producers = Producers::parse(
        json!([{"name": PRODUCER, "key": key, "checks": [name]}])
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let trust = Trust {
        signer: "acme/ci/.github/workflows/gate.yml".parse().unwrap(),
        fresh: None,
        producers,
    };
    let run: conary_github::api::CheckRun = serde_json::from_value(json!({
        "id": 1, "name": name, "status": "completed", "conclusion": conclusion,
        "html_url": null, "started_at": null, "check_suite": null,
        "output": {"text": text}}))
    .unwrap();
    let github =
        conary_github::Api::new("http://127.0.0.1:1", conary_github::Tokens::given("unused"));
    let repo: Repo = REPO.parse().unwrap();
    let definition = flow::recipe_digest(recipe);
    attested(
        &github,
        &github,
        &repo,
        Subject::Commit {
            commit,
            tree,
            definition: Some(&definition),
        },
        &run,
        &trust,
    )
    .unwrap()
    .map(|a| (a.result, a.log))
}

#[test]
fn the_plugin_is_a_program_and_conary_refuses_one_that_offers_a_model() {
    let Answer::Describe {
        models,
        capabilities,
        meters,
        kinds,
        v4_kinds,
        ..
    } = describe()
    else {
        panic!()
    };
    assert!(models.is_empty());
    let kinds: Vec<Kind> = kinds.into_iter().chain(v4_kinds).collect();
    let offers = |models| Offers {
        models,
        capabilities: capabilities.clone().unwrap(),
        meters: meters.clone(),
    };
    assert!(offers(vec![]).valid(&kinds));
    let model = Model {
        id: "a-model".into(),
        efforts: vec!["high".into()],
        context_tokens: None,
        max_output_tokens: None,
    };
    assert!(!offers(vec![model]).valid(&kinds), "a proof is a program");
}

#[test]
fn a_pass_is_signed_for_its_commit_and_its_recipe_alone() {
    let r = rig();
    r.script(&[Ended::Exited(PASSED)]);
    let got = r
        .ask(&request("proof", &r.head, &r.tree, &["AGENTS"]))
        .unwrap();
    assert_eq!(got["results"][0]["check"], "AGENTS", "{got}");
    assert_eq!(got["results"][0]["verdict"], "pass", "{got}");
    assert_eq!(got["head"], r.head.as_str());
    // The worker ran the flow's recipe on the commit, under the settings'
    // name for the check, and its unit is let go of.
    let recipe = Recipe::Build("checks.x86_64-linux.agents".into());
    let started = r.started();
    assert_eq!(started.len(), 1);
    assert_eq!(
        (
            started[0].mode,
            &*started[0].check,
            &started[0].recipe,
            &*started[0].head
        ),
        (Mode::Proof, "agents", &recipe, r.head.as_str())
    );
    assert_eq!(r.stopped().len(), 1);
    let runs = r.runs();
    assert_eq!(runs[0].0, "start agents");
    assert_eq!(runs[1].0, "success");
    let text = runs[1].1.clone().unwrap();
    let (outcome, log) = verify(&text, "success", "agents", &r.head, &r.tree, &recipe).unwrap();
    assert_eq!(outcome, Outcome::Pass);
    assert_eq!(log.as_deref(), Some(&*flow::receipt_digest(b"built\n")));
    // A recipe changed since fails Conary's definition check.
    let changed = Recipe::Build("checks.x86_64-linux.other".into());
    assert!(verify(&text, "success", "agents", &r.head, &r.tree, &changed).is_err());
    // Its run folder is gone.
    assert_eq!(
        std::fs::read_dir(r.cfg.state.join("runs")).unwrap().count(),
        0
    );
}

#[test]
fn a_fail_is_signed_as_one() {
    let r = rig();
    r.script(&[Ended::Exited(FAILED)]);
    let got = r
        .ask(&request("proof", &r.head, &r.tree, &["gate"]))
        .unwrap();
    assert_eq!(got["results"][0]["verdict"], "fail", "{got}");
    let runs = r.runs();
    assert_eq!(runs[1].0, "failure");
    // Signed as a fail, which Conary counts as no pass, whatever run
    // carries it.
    let recipe = Recipe::Run(vec!["bash".into(), "-c".into(), "make gate".into()]);
    let text = runs[1].1.as_deref().unwrap();
    for conclusion in ["failure", "success"] {
        let why = verify(text, conclusion, "gate", &r.head, &r.tree, &recipe).unwrap_err();
        assert!(why.contains("says fail"), "{why}");
    }
}

#[test]
fn a_tree_that_isnt_the_heads_is_refused_before_anything_runs() {
    let r = rig();
    let other = "f".repeat(40);
    let why = r
        .ask(&request("proof", &r.head, &other, &["agents"]))
        .unwrap_err();
    assert!(why.contains("isn't") && why.contains("tree"), "{why}");
    assert!(r.started().is_empty());
    assert!(r.runs().is_empty(), "no check run");
}

#[test]
fn a_job_recipe_or_a_check_with_none_is_refused_before_anything_runs() {
    let r = rig();
    for check in ["think", "unknown"] {
        let why = r
            .ask(&request("proof", &r.head, &r.tree, &["agents", check]))
            .unwrap_err();
        assert!(why.contains(check), "{why}");
    }
    let why = r
        .ask(&request("prepare", &r.head, &r.tree, &["think"]))
        .unwrap_err();
    assert!(why.contains("a proof is a program"), "{why}");
    assert!(r.started().is_empty());
    assert!(r.runs().is_empty());
}

#[test]
fn a_run_with_no_verdict_signs_nothing_and_cancels_its_check_run() {
    let r = rig();
    r.script(&[Ended::Broke("killed".into()), Ended::Exited(BROKE)]);
    for _ in 0..2 {
        let why = r
            .ask(&request("proof", &r.head, &r.tree, &["agents"]))
            .unwrap_err();
        assert!(why.contains("agents"), "{why}");
    }
    let runs = r.runs();
    assert_eq!(
        runs.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["start agents", "cancelled", "start agents", "cancelled"]
    );
    assert!(runs.iter().all(|r| r.1.is_none()), "nothing signed");
    assert_eq!(r.stopped().len(), 2);
}

#[test]
fn an_abandoned_request_stops_its_unit_and_publishes_no_success() {
    let r = rig();
    r.units.0.lock().unwrap().hang = true;
    let served = r.serve(1);
    let mut stream = UnixStream::connect(&r.cfg.socket).unwrap();
    let mut line = request("proof", &r.head, &r.tree, &["agents"]).to_string();
    line.push('\n');
    stream.write_all(line.as_bytes()).unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    while r.started().is_empty() && Instant::now() < until {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(r.started().len(), 1);
    // The plugin is gone: killed, past its deadline, cancelled.
    drop(stream);
    served.join().unwrap();
    assert_eq!(r.stopped().len(), 1, "its unit is stopped");
    let runs = r.runs();
    assert_eq!(
        runs.iter().map(|r| r.0.as_str()).collect::<Vec<_>>(),
        ["start agents", "cancelled"]
    );
}

#[test]
fn another_commit_with_the_same_tree_runs_again_and_is_signed_for_itself() {
    let r = rig();
    r.script(&[Ended::Exited(PASSED), Ended::Exited(PASSED)]);
    r.ask(&request("proof", &r.head, &r.tree, &["agents"]))
        .unwrap();
    r.ask(&request("proof", &r.twin, &r.tree, &["agents"]))
        .unwrap();
    let started = r.started();
    assert_eq!(
        started.iter().map(|s| s.head.as_str()).collect::<Vec<_>>(),
        [r.head.as_str(), r.twin.as_str()],
        "it ran again"
    );
    let recipe = Recipe::Build("checks.x86_64-linux.agents".into());
    let texts: Vec<String> = r.runs().into_iter().filter_map(|r| r.1).collect();
    assert_eq!(texts.len(), 2);
    assert!(verify(&texts[1], "success", "agents", &r.twin, &r.tree, &recipe).is_ok());
    // The first commit's statement never counts for the second.
    assert!(verify(&texts[0], "success", "agents", &r.twin, &r.tree, &recipe).is_err());
    assert_ne!(texts[0], texts[1]);
}

#[test]
fn a_prepare_posts_no_check_run() {
    let r = rig();
    r.script(&[Ended::Exited(PASSED)]);
    let got = r
        .ask(&request("prepare", &r.head, &r.tree, &["agents"]))
        .unwrap();
    assert_eq!(
        (got["ready"].clone(), got["slot"].clone()),
        (json!(true), json!(false)),
        "{got}"
    );
    r.script(&[Ended::Exited(FAILED)]);
    let got = r
        .ask(&request("prepare", &r.head, &r.tree, &["agents", "gate"]))
        .unwrap();
    assert_eq!(
        (got["ready"].clone(), got["slot"].clone()),
        (json!(true), json!(true)),
        "{got}"
    );
    r.script(&[Ended::Broke("killed".into())]);
    let got = r
        .ask(&request("prepare", &r.head, &r.tree, &["agents"]))
        .unwrap();
    assert_eq!(got["ready"], false, "{got}");
    // A Run recipe's prepare starts no unit: there's nothing to build.
    assert!(
        r.started()
            .iter()
            .all(|s| s.mode == Mode::Prepare && s.check == "agents")
    );
    assert_eq!(r.started().len(), 3);
    assert!(r.runs().is_empty(), "{:?}", r.runs());
}

#[test]
fn a_v1_proof_names_no_commit_and_is_refused() {
    let r = rig();
    let mut asked = request("proof", &r.head, &r.tree, &["agents"]);
    asked["contract"] = json!(conary_core::plugin::CONTRACT_V3);
    let why = r.ask(&asked).unwrap_err();
    assert!(why.contains("conary-plugin v4"), "{why}");
}

#[test]
fn startup_stops_what_a_restart_left_and_withdraws_unfinished_runs() {
    let r = rig();
    r.units.0.lock().unwrap().loaded = vec!["abc".into()];
    // Its folder, and another whose unit a stop already ended.
    std::fs::create_dir_all(r.cfg.run("abc")).unwrap();
    std::fs::create_dir_all(r.cfg.run("def").join("repo.git")).unwrap();
    // A proof whose check run a crash left in progress, with no result.
    let records = Records::open(&r.cfg.state.join("records")).unwrap();
    let mut record = records
        .begin::<Proof>(Target {
            repo: REPO.into(),
            head: r.head.clone(),
            check: "agents".into(),
        })
        .unwrap();
    publish::start(&mut r.github.clone(), &records, &mut record).unwrap();
    assert_eq!(r.producer.reconcile().unwrap(), 1);
    assert_eq!(r.stopped(), ["abc"]);
    assert!(!r.cfg.run("abc").exists() && !r.cfg.run("def").exists());
    assert_eq!(
        r.runs(),
        [("start agents".into(), None), ("cancelled".into(), None)]
    );
    assert_eq!(r.producer.reconcile().unwrap(), 0, "finished once");
}

#[test]
fn only_an_exit_systemd_kept_as_the_units_success_is_one() {
    let shown = |active: &str, sub: &str, result: &str, code: &str, status: &str| {
        ended(&format!(
            "ActiveState={active}\nSubState={sub}\nResult={result}\nExecMainCode={code}\nExecMainStatus={status}\n"
        ))
    };
    assert_eq!(
        shown("active", "exited", "success", "1", "0"),
        Ended::Exited(0)
    );
    assert_eq!(
        shown("active", "exited", "success", "1", "1"),
        Ended::Exited(1)
    );
    // Whatever its state says, a result systemd didn't take as success.
    assert!(matches!(
        shown("active", "exited", "exit-code", "1", "0"),
        Ended::Broke(_)
    ));
    // Killed: a signal's status is no exit.
    assert!(matches!(
        shown("active", "exited", "success", "2", "9"),
        Ended::Broke(_)
    ));
    assert!(matches!(
        shown("failed", "failed", "exit-code", "1", "2"),
        Ended::Broke(_)
    ));
    assert!(matches!(
        shown("failed", "failed", "timeout", "2", "15"),
        Ended::Broke(_)
    ));
    // Collected, or never loaded.
    assert!(matches!(
        shown("inactive", "dead", "success", "0", "0"),
        Ended::Broke(_)
    ));
    assert!(matches!(ended(""), Ended::Broke(_)));
}

/// No cgroups: each command runs as it is, and clearing does nothing.
struct Bare;

impl Fence for Bare {
    fn around(&mut self, _: usize, program: &Path, args: &[&str]) -> std::io::Result<Command> {
        let mut c = Command::new(program);
        c.args(args);
        Ok(c)
    }
    fn clear(&mut self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn record(&self) -> Option<PathBuf> {
        None
    }
}

#[test]
fn a_run_recipe_is_its_command_in_a_fresh_clone_and_its_exit_the_verdict() {
    let r = rig();
    // The producer's mirror, as a fetch leaves it.
    r.ask(&request("prepare", &r.head, &r.tree, &["gate"]))
        .unwrap();
    let run = |argv: &[&str], tree: &str| {
        let id = format!("{:x}", rand_ish());
        let spec = Spec {
            mode: Mode::Proof,
            repo: REPO.into(),
            head: r.head.clone(),
            tree: tree.into(),
            check: "gate".into(),
            recipe: Recipe::Run(argv.iter().map(|c| (*c).to_owned()).collect()),
        };
        // Laid out as the producer lays a run out: the worker reads its
        // own copy of the repo, never the mirror.
        lay(r.cfg.bench(), &r.cfg.mirror(REPO), &id, &spec).unwrap();
        std::fs::rename(r.cfg.mirror(REPO), r.dir.path().join("hidden")).unwrap();
        let code = work::work(r.cfg.bench(), &id, &mut Bare);
        let log = std::fs::read_to_string(r.cfg.log(&id)).unwrap();
        std::fs::rename(r.dir.path().join("hidden"), r.cfg.mirror(REPO)).unwrap();
        (code, log)
    };
    let bash = |script: &'static str| ["bash", "-c", script];
    let (code, log) = run(
        &bash("test -f README && echo $PROOF_CHECK on $PROOF_HEAD"),
        &r.tree,
    );
    assert_eq!(code, PASSED, "{log}");
    assert!(log.contains(&format!("gate on {}", r.head)), "{log}");
    // Its home is the run's own, which it may write: nix and cargo cache
    // there, and nix refuses a home that isn't its user's.
    let (code, log) = run(
        &bash(r#"[[ $HOME == */*.home ]] && test -O "$HOME" && touch "$HOME/cached""#),
        &r.tree,
    );
    assert_eq!(code, PASSED, "{log}");
    assert_eq!(run(&bash("exit 3"), &r.tree).0, FAILED);
    assert_eq!(run(&["false"], &r.tree).0, FAILED);
    assert_eq!(
        run(&bash("kill -9 $$"), &r.tree).0,
        BROKE,
        "a signal is no verdict"
    );
    assert_eq!(run(&bash("exit 127"), &r.tree).0, BROKE);
    assert_eq!(run(&["/nonexistent/program"], &r.tree).0, BROKE);
    assert_eq!(run(&["true"], &"e".repeat(40)).0, BROKE, "another tree");
    assert_eq!(run(&[], &r.tree).0, BROKE);
    // Its clones and homes are gone.
    assert_eq!(std::fs::read_dir(&r.cfg.trees).unwrap().count(), 0);
    let _ = r.dir;
}

fn rand_ish() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}

#[test]
fn the_capabilities_are_a_programs() {
    let Answer::Describe { capabilities, .. } = describe() else {
        panic!()
    };
    assert_eq!(
        capabilities,
        Some(Capabilities {
            sandbox: Sandbox::Brings,
            network: true,
            devices: vec![],
            mcp: false,
        })
    );
}

#[test]
fn a_runs_id_names_a_user_of_its_own() {
    // systemd's user names: at most 31 characters, `proof-` and the id.
    let ids: Vec<String> = (0..1000).map(|_| run_id()).collect();
    for id in &ids {
        assert!(format!("proof-{id}").len() <= 31, "{id}");
        assert!(id.bytes().all(|b| b.is_ascii_hexdigit()), "{id}");
    }
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len());
}

#[test]
fn a_build_fails_when_nix_says_a_builder_in_it_did() {
    // nix-store's own words through the daemon, which exits 1 whatever
    // failed.
    let said = |failed: &str, reason: &str| {
        format!(
            "building '{failed}'...\n\
             error: Cannot build '{failed}'.\n       Reason: {reason}.\n       Output paths:\n         /nix/store/x-out\n\
             error: Cannot build '/nix/store/t-top.drv'.\n       Reason: 1 dependency failed.\n\
             error: Build failed due to failed dependency\n"
        )
    };
    let built = r#"Derive([("out","/nix/store/b-bad","","")],[],[],"x86_64-linux","/bin/sh",["-c","exit 2"],[("out","/nix/store/b-bad")])"#;
    let fetched = r#"Derive([("out","/nix/store/f-fetch","sha256","e3b0c44298fc1c149afbf4e35bc078b4c539668989de3cdcde285867c7dbb083")],[],[],"x86_64-linux","/bin/sh",["-c","exit 1"],[("out","/nix/store/f-fetch")])"#;
    let read = |drv: &str| match drv {
        "/nix/store/b-bad.drv" => Ok(built.to_owned()),
        "/nix/store/f-fetch.drv" => Ok(fetched.to_owned()),
        _ => Err(std::io::Error::other("not there")),
    };
    let builder = "builder failed with exit code 2";
    // A builder of the check's own, there or in what it needs: the verdict.
    assert_eq!(
        work::failed(&said("/nix/store/b-bad.drv", builder), read),
        Ok(true)
    );
    // A fetch failing is the network's.
    assert_eq!(
        work::failed(&said("/nix/store/f-fetch.drv", builder), read),
        Ok(false)
    );
    // And anything but a builder failing is nix's.
    assert_eq!(
        work::failed(
            &said("/nix/store/b-bad.drv", "timed out after 60 seconds"),
            read
        ),
        Ok(false)
    );
    assert_eq!(
        work::failed("error: cannot connect to socket", read),
        Ok(false)
    );
    let full = said("/nix/store/b-bad.drv", builder)
        + "note: build failure may have been caused by lack of free disk space\n";
    assert_eq!(work::failed(&full, read), Ok(false));
    // A derivation nix names that can't be read is no verdict.
    assert!(work::failed(&said("/nix/store/g-gone.drv", builder), read).is_err());
}

#[test]
fn build_derivation_eval_runs_in_heavy_slot() {
    use std::os::unix::fs::PermissionsExt;
    let r = rig();
    r.ask(&request("prepare", &r.head, &r.tree, &["gate"]))
        .unwrap();
    let bin = r.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let seen = r.dir.path().join("heavy.log");
    let script = |name: &str, body: &str| {
        let p = bin.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let heavy = script(
        "heavy",
        &format!(
            "printf '%s\\n' \"$*\" >> '{}'\nshift\nexec \"$@\"",
            seen.display()
        ),
    );
    script("nix", "echo /nix/store/fake-agents.drv");
    script(
        "nix-store",
        "case \"$1\" in --query) echo /nix/store/fake-out ;; --check-validity) ;; esac",
    );
    let mut cfg = r.cfg.clone();
    cfg.nix = bin;
    cfg.heavy = Some(vec![heavy.to_string_lossy().into_owned(), "--".into()]);
    let id = format!("{:x}", rand_ish());
    let spec = Spec {
        mode: Mode::Prepare,
        repo: REPO.into(),
        head: r.head.clone(),
        tree: r.tree.clone(),
        check: "agents".into(),
        recipe: Recipe::Build("checks.x86_64-linux.agents".into()),
    };
    lay(cfg.bench(), &cfg.mirror(REPO), &id, &spec).unwrap();
    let code = work::work(cfg.bench(), &id, &mut Bare);
    let log = std::fs::read_to_string(cfg.log(&id)).unwrap();
    assert_eq!(code, PASSED, "{log}");
    let ran = std::fs::read_to_string(&seen).unwrap_or_default();
    assert!(
        ran.contains("eval") && ran.contains("checks.x86_64-linux.agents.drvPath"),
        "expected nix eval to run through heavy, got: {ran:?}"
    );
}

#[test]
fn concurrent_build_prepares_for_one_repo_serialize_and_abandon_cleanly() {
    let r = rig();
    {
        let mut fake = r.github.0.lock().unwrap();
        for commit in [&r.head, &r.twin] {
            fake.on(
                "GET",
                &format!("repos/{REPO}/git/commits/{commit}"),
                (0..4)
                    .map(|_| ok(200, json!({"tree": {"sha": r.tree}}), &[]))
                    .collect(),
            );
        }
    }
    let gate = Arc::new(Gate::default());
    {
        let mut s = r.units.0.lock().unwrap();
        s.gate = Some(gate.clone());
        s.ends = [Ended::Exited(PASSED), Ended::Exited(PASSED)].into();
    }
    let me = Controller(rustix::process::getuid().as_raw());
    let server = Server::bind(&r.cfg.socket, me).unwrap();
    let producer = r.producer.clone();
    thread::spawn(move || {
        let _ = server.serve(move |req: Request, w: &Work| producer.handle(req, w));
    });
    let call = |cfg: Config, req: Value| {
        thread::spawn(move || {
            let mut out = Vec::new();
            plugin(&cfg)
                .answer(&mut req.to_string().as_bytes(), &mut out)
                .map(|()| serde_json::from_slice::<Value>(&out).unwrap())
        })
    };
    let c1 = call(
        r.cfg.clone(),
        request("prepare", &r.head, &r.tree, &["agents"]),
    );
    // Wait until the first prepare enters its unit.
    {
        let mut g = gate.state.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while g.0 == 0 && Instant::now() < deadline {
            let (guard, _) = gate
                .changed
                .wait_timeout(g, Duration::from_millis(20))
                .unwrap();
            g = guard;
        }
        assert_eq!(g.0, 1, "first prepare should be active");
    }
    let c2 = call(
        r.cfg.clone(),
        request("prepare", &r.twin, &r.tree, &["agents"]),
    );
    // A third prepare arrives and abandons while waiting behind the first.
    let mut c3 = UnixStream::connect(&r.cfg.socket).unwrap();
    let mut line = request("prepare", &r.head, &r.tree, &["agents"]).to_string();
    line.push('\n');
    c3.write_all(line.as_bytes()).unwrap();
    thread::sleep(Duration::from_millis(80));
    drop(c3);
    thread::sleep(Duration::from_millis(80));
    {
        let mut g = gate.state.lock().unwrap();
        assert_eq!(g.1, 1, "at most one build prepare unit per repo at a time");
        g.2 = true;
        gate.changed.notify_all();
    }
    let r1 = c1.join().unwrap().unwrap();
    let r2 = c2.join().unwrap().unwrap();
    assert_eq!(
        (r1["ready"].clone(), r1["slot"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(
        (r2["ready"].clone(), r2["slot"].clone()),
        (json!(true), json!(false))
    );
    assert_eq!(gate.state.lock().unwrap().1, 1);
    assert_eq!(
        r.started().len(),
        2,
        "abandoned prepare never started a unit"
    );
}

#[test]
fn concurrent_fetches_for_same_repo_serialize_mirror_init() {
    use std::os::unix::fs::PermissionsExt;
    let r = rig();
    // Wrap git to detect any two processes touching the repo's mirror at once.
    let bin = r.dir.path().join("git-wrap");
    std::fs::create_dir_all(&bin).unwrap();
    let guard = r.dir.path().join("mirror.busy");
    let git_wrap = bin.join("git");
    std::fs::write(
        &git_wrap,
        format!(
            "#!/bin/sh\n\
             if ! mkdir '{g}' 2>/dev/null; then\n\
               echo 'concurrent git on mirror' >&2\n\
               exit 99\n\
             fi\n\
             trap \"rmdir '{g}'\" EXIT\n\
             sleep 0.05\n\
             git \"$@\"\n",
            g = guard.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&git_wrap, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut cfg = r.cfg.clone();
    cfg.git = git_wrap;
    {
        let mut fake = r.github.0.lock().unwrap();
        fake.on(
            "GET",
            &format!("repos/{REPO}/git/commits/{}", r.head),
            (0..8)
                .map(|_| ok(200, json!({"tree": {"sha": r.tree}}), &[]))
                .collect(),
        );
    }
    let shared = r.github.clone();
    let github: GitHub = Box::new(move || Box::new(shared.clone()));
    let producer = Arc::new(
        Producer::new(
            cfg.clone(),
            github,
            Box::new(r.units.clone()),
            Signer::for_tests(PRODUCER, SECRET),
        )
        .unwrap(),
    );
    let me = Controller(rustix::process::getuid().as_raw());
    let server = Server::bind(&cfg.socket, me).unwrap();
    let served = producer.clone();
    thread::spawn(move || {
        let _ = server.serve(move |req: Request, w: &Work| served.handle(req, w));
    });
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let cfg = cfg.clone();
            let req = request("prepare", &r.head, &r.tree, &["gate"]);
            thread::spawn(move || {
                let mut out = Vec::new();
                plugin(&cfg)
                    .answer(&mut req.to_string().as_bytes(), &mut out)
                    .map(|()| serde_json::from_slice::<Value>(&out).unwrap())
            })
        })
        .collect();
    for h in handles {
        let got = h.join().unwrap().unwrap();
        assert_eq!(
            (got["ready"].clone(), got["slot"].clone()),
            (json!(true), json!(true)),
            "{got}"
        );
    }
}
