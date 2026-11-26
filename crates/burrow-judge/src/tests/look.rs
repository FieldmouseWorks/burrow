//! A judgment's look: the producer lays it out from its own mirror, opens
//! its gate and runs it, here in this process through a stand-in for
//! Claude Code, the worker's real work but for systemd (the VM test,
//! tests/judge-look.nix, is systemd's and the users' half).
use super::build::Builds;
use super::world::{World, github, read, world};
use super::{PRODUCER, REPO, SECRET, ask, ask_with, config, judge_request, seal};
use crate::EngineConfig;
use crate::egress::Dial;
use crate::look::{ANSWER, Looked, Looks, Spec};
use crate::producer::{Judges, Producer};
use crate::worker::{
    JUDGE_DIR_ENV, JUDGE_SCHEMA_ENV, Worker, holds_login, looked, plugin_request, schema,
};
use burrow_producer::serve::Work;
use burrow_producer::sign::Signer;
use burrow_producer::units::{Ended, Units};
use conary_core::plugin::{Judgment, Request, Trigger};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

/// What a stand-in Conary v5 engine plugin prints when the test doesn't say:
/// a fresh-session job response carrying the structured verdict JSON.
pub(super) fn printed_for(engine: &str, verdict: &str, model: &str, effort: Option<&str>) -> Value {
    json!({
        "contract": conary_core::plugin::CONTRACT,
        "by": {"engine": engine, "session": "look-session"},
        "execution": {"model": model, "effort": effort, "context": {"kind": "fresh"}},
        "kind": "job",
        "summary": json!({"verdict": verdict, "why": "Read the diff and every thread."}).to_string(),
        "next": {"step": "done"},
    })
}

pub(super) fn printed(verdict: &str, model: &str) -> Value {
    printed_for("claude", verdict, model, Some("high"))
}

/// bash, by its path on PATH: a build's sandbox has no /usr/bin/env, and
/// the stand-in needs bash's own /dev/tcp.
fn bash() -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is set");
    std::env::split_paths(&path)
        .map(|d| d.join("bash"))
        .find(|b| b.is_file())
        .expect("bash is on PATH")
}

/// A stand-in Conary v5 engine plugin in `dir`: it keeps its request JSON,
/// where it ran, and its environment in `dir/seen`, probes its egress proxy
/// for a disallowed host and its API host, and prints `dir/prints`.
fn stand_in_for(dir: &Path, engine: &str, probe_hosts: &[&str]) -> PathBuf {
    let seen = dir.join("seen");
    std::fs::create_dir_all(&seen).unwrap();
    let path = dir.join(format!("{engine}-plugin"));
    let probes = probe_hosts
        .iter()
        .map(|h| format!("ask {h}"))
        .collect::<Vec<_>>()
        .join("\n");
    burrow_test::executable(
        &path,
        format!(
            r#"#!{bash}
seen={seen}
pwd >$seen/pwd
env >$seen/env
cat >$seen/request
ask() {{
  hostport=${{HTTPS_PROXY#http://}}
  exec 3<>/dev/tcp/${{hostport%:*}}/${{hostport#*:}}
  printf 'CONNECT %s HTTP/1.1\r\n\r\n' "$1" >&3
  read -r line <&3
  echo "$line" >>$seen/proxy
  exec 3>&-
}}
{probes}
cat {prints}
"#,
            bash = bash().display(),
            seen = seen.display(),
            prints = dir.join("prints").display(),
        ),
    );
    path
}

fn stand_in(dir: &Path) -> PathBuf {
    let path = stand_in_for(dir, "claude", &["example.org:443", "api.anthropic.com:443"]);
    std::fs::write(dir.join("prints"), printed("pass", "a-model").to_string()).unwrap();
    path
}

/// Units that run each look's worker in this process, as its unit would,
/// and keep what they were asked.
#[derive(Clone)]
struct InProcess {
    engines: BTreeMap<String, EngineConfig>,
    credentials: BTreeMap<String, Vec<u8>>,
    looks: PathBuf,
    home: PathBuf,
    asked: Arc<Mutex<Asked>>,
}

#[derive(Default)]
pub(super) struct Asked {
    pub ran: Vec<String>,
    pub stopped: Vec<String>,
    pub loaded: Vec<String>,
    /// What each look's folder held when it ran: its spec, and its head's
    /// and its base's files.
    pub laid: Vec<(Spec, String, String, String)>,
    /// Whether the head's link was laid out as a plain file.
    link_is_file: Vec<bool>,
    /// Whether a look's request ends while it runs.
    abandon: bool,
}

impl Units for InProcess {
    fn run(&self, id: &str, _: &Work, _: Instant) -> Result<Ended, String> {
        let dir = self.looks.join(id);
        let read = |p: &str| std::fs::read_to_string(dir.join(p)).unwrap_or_default();
        let spec: Spec = serde_json::from_str(&read("spec.json")).unwrap();
        self.asked.lock().unwrap().laid.push((
            spec,
            read("head/src/lib.rs"),
            read("base/AGENTS.md"),
            read("changes.diff"),
        ));
        self.asked.lock().unwrap().ran.push(id.into());
        let link = std::fs::symlink_metadata(dir.join("head/leak")).unwrap();
        self.asked.lock().unwrap().link_is_file.push(link.is_file());
        if self.asked.lock().unwrap().abandon {
            return Err("its request was abandoned".into());
        }
        let creds = self.credentials.clone();
        let worker = Worker {
            engines: self.engines.clone(),
            dir,
            home: self.home.clone(),
            read_credential: Box::new(move |name| {
                creds
                    .get(name)
                    .cloned()
                    .ok_or_else(|| format!("missing test credential {name}"))
            }),
        };
        Ok(match worker.look() {
            Ok(()) => Ended::Exited(0),
            Err(why) => {
                eprintln!("{why}");
                Ended::Exited(1)
            }
        })
    }
    fn stop(&self, id: &str) -> Result<(), String> {
        self.asked.lock().unwrap().stopped.push(id.into());
        Ok(())
    }
    fn loaded(&self) -> Result<Vec<String>, String> {
        Ok(self.asked.lock().unwrap().loaded.clone())
    }
}

/// `REPO` on the scripted GitHub's side, in `dir/github`: a base with its
/// rules and a head on it, which the world's PR is made to name.
fn repo(dir: &Path, w: &World) {
    let work = dir.join("work");
    std::fs::create_dir_all(work.join("src")).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(&work)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    };
    git(&["init", "-q"]);
    std::fs::write(work.join("AGENTS.md"), "The base's rules.\n").unwrap();
    std::fs::write(work.join("src/lib.rs"), "fn old() {}\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::write(work.join("src/lib.rs"), "fn new() {}\n").unwrap();
    // A PR doesn't get to rewrite its rules: the look reads the base's.
    std::fs::write(work.join("AGENTS.md"), "Anything goes.\n").unwrap();
    // A link to what the look's user could read, which it never follows.
    std::os::unix::fs::symlink("/proc/self/environ", work.join("leak")).unwrap();
    // A file LFS keeps, which stays its pointer.
    std::fs::write(work.join(".gitattributes"), "*.bin filter=lfs -text\n").unwrap();
    std::fs::write(
        work.join("big.bin"),
        "version https://git-lfs.github.com/spec/v1\n",
    )
    .unwrap();
    git(&["add", "."]);
    git(&["commit", "-qm", "head"]);
    let head = git(&["rev-parse", "HEAD"]);
    let bare = dir.join("github").join(format!("{REPO}.git"));
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    git(&["clone", "-q", "--bare", ".", bare.to_str().unwrap()]);
    // The producer's mirror, with an LFS filter as the App's environment
    // names one (github.nix): it fails as git-lfs does with no endpoint to
    // fetch from, unless told to leave pointers be.
    let mirror = dir.join("state/git").join(format!("{REPO}.git"));
    std::fs::create_dir_all(&mirror).unwrap();
    git(&["init", "-q", "--bare", mirror.to_str().unwrap()]);
    for (key, value) in [
        (
            "filter.lfs.smudge",
            r#"sh -c 'test -n "$GIT_LFS_SKIP_SMUDGE" && exec cat; exit 1'"#,
        ),
        ("filter.lfs.required", "true"),
    ] {
        git(&["--git-dir", mirror.to_str().unwrap(), "config", key, value]);
    }
    w.with(|p| {
        p.head = head;
        p.base_sha = base;
    });
}

/// Where the gates dial, and whom: a listener that greets each caller.
fn api(dialed: Arc<Mutex<Vec<String>>>) -> Dial {
    let srv = TcpListener::bind("127.0.0.1:0").unwrap();
    let at = srv.local_addr().unwrap();
    thread::spawn(move || {
        for c in srv.incoming().flatten() {
            let mut c = c;
            let _ = c.write_all(b"hello");
        }
    });
    Arc::new(move |host: &str| {
        dialed.lock().unwrap().push(host.into());
        TcpStream::connect(at)
    })
}

pub(super) struct Rig {
    pub dir: tempfile::TempDir,
    pub w: World,
    units: InProcess,
    pub builds: Builds,
    dialed: Arc<Mutex<Vec<String>>>,
}

pub(super) fn rig() -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let w = world();
    repo(dir.path(), &w);
    let home = dir.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut engines = BTreeMap::new();
    engines.insert(
        "claude".into(),
        EngineConfig {
            plugin: stand_in(dir.path()),
            unit: "autoland-judge-look-claude".into(),
            credential: "judge-login-claude".into(),
            credential_env: "CLAUDE_CODE_OAUTH_TOKEN".into(),
            hosts: vec!["api.anthropic.com".into()],
        },
    );
    let credentials = BTreeMap::from([("judge-login-claude".into(), b"the-look-token\n".to_vec())]);
    let units = InProcess {
        engines,
        credentials,
        looks: dir.path().join("state/looks"),
        home,
        asked: Arc::default(),
    };
    let builds = Builds::new(config(dir.path()));
    Rig {
        dir,
        w,
        units,
        builds,
        dialed: Arc::default(),
    }
}

impl Rig {
    pub fn looks(&self) -> Looks {
        let units: BTreeMap<String, Box<dyn Units>> = BTreeMap::from([(
            "claude".into(),
            Box::new(self.units.clone()) as Box<dyn Units>,
        )]);
        Looks::new(
            config(self.dir.path()),
            read(&self.w),
            units,
            Box::new(self.builds.clone()),
            api(self.dialed.clone()),
        )
        .unwrap()
    }

    pub fn producer(&self) -> Producer {
        Producer::new(
            config(self.dir.path()),
            read(&self.w),
            github(&self.w),
            Box::new(self.looks()),
            Signer::for_tests(PRODUCER, SECRET),
            seal(),
        )
        .unwrap()
    }

    pub fn seen(&self, name: &str) -> String {
        if name == "prompt" {
            let raw =
                std::fs::read_to_string(self.dir.path().join("seen/request")).unwrap_or_default();
            return serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|v| v.get("prompt")?.as_str().map(str::to_owned))
                .unwrap_or_default();
        }
        std::fs::read_to_string(self.dir.path().join("seen").join(name)).unwrap_or_default()
    }

    pub fn prints(&self, printed: &Value) {
        std::fs::write(self.dir.path().join("prints"), printed.to_string()).unwrap();
    }

    /// A look's judgment of `request`, asked as the producer asks it.
    pub fn judge(&self, request: &Request) -> Result<Value, String> {
        let looks = self.looks();
        let dir = self.dir.path().join("asking");
        std::fs::create_dir_all(&dir).unwrap();
        ask_with(
            &dir,
            &|q: Request, k: &Work| looks.judge(&q, k),
            request,
            None,
        )
    }

    pub fn asked(&self) -> std::sync::MutexGuard<'_, Asked> {
        self.units.asked.lock().unwrap()
    }
}

#[test]
fn a_look_judges_the_laid_out_head_and_is_published_by_its_own_session_and_model() {
    let r = rig();
    let p = r.producer();
    let request = judge_request(&r.w, vec![Trigger::Asked]);
    let got = ask(r.dir.path(), &p, &request).unwrap();
    assert_eq!(got["verdict"], "pass", "{got}");
    assert_eq!(
        got["by"],
        json!({"engine": "claude", "session": "look-session"})
    );
    assert_eq!(got["execution"]["model"], "a-model");
    assert_eq!(got["execution"]["effort"], "high");
    assert_eq!(r.w.posted().len(), 1);
    assert_eq!(r.w.completed().len(), 1);

    let asked = r.asked();
    let [id] = asked.ran.as_slice() else {
        panic!("{:?}", asked.ran)
    };
    // Stopped once it ended, and its folder gone.
    assert_eq!(asked.stopped, std::slice::from_ref(id));
    assert!(!r.dir.path().join("state/looks").join(id).exists());
    // What it was laid out with: the head's code, the base's rules, and
    // the diff between them, from the producer's own fetch.
    let (spec, head, rules, diff) = &asked.laid[0];
    assert_eq!(head, "fn new() {}\n");
    // The head's link is a file holding where it points.
    assert_eq!(asked.link_is_file, [true]);
    assert_eq!(rules, "The base's rules.\n");
    assert!(diff.contains("+fn new() {}"), "{diff}");
    assert_eq!(
        (spec.model.as_str(), spec.effort.as_deref()),
        ("a-model", Some("high"))
    );

    // The engine plugin, as the look runs it: fresh Conary v5 Task::Job,
    // its folder via BURROW_JUDGE_DIR, strict verdict schema via
    // BURROW_JUDGE_JSON_SCHEMA, from a home of its own, its login the
    // unit's, and its proxy the gate.
    let dir = r.dir.path().join("state/looks").join(id);
    let sent: Request = serde_json::from_str(&r.seen("request")).unwrap();
    assert_eq!(
        serde_json::to_value(&sent).unwrap(),
        serde_json::to_value(plugin_request(spec, &r.units.home)).unwrap()
    );
    let home = r.units.home.to_string_lossy().into_owned();
    assert_eq!(r.seen("pwd").trim(), home);
    let env = r.seen("env");
    assert!(env.lines().any(|l| l == format!("HOME={home}")), "{env}");
    assert!(
        env.lines()
            .any(|l| l == "CLAUDE_CODE_OAUTH_TOKEN=the-look-token"),
        "{env}"
    );
    assert!(
        env.lines()
            .any(|l| l == format!("{JUDGE_DIR_ENV}={}", dir.display())),
        "{env}"
    );
    assert!(
        env.lines()
            .any(|l| l == format!("{JUDGE_SCHEMA_ENV}={}", schema())),
        "{env}"
    );
    assert!(
        env.lines()
            .any(|l| l.starts_with("HTTPS_PROXY=http://127.0.0.1:")),
        "{env}"
    );
    // Nothing of the producer's environment: its App, its credentials.
    let given = [
        "PATH",
        "HOME",
        "TMPDIR",
        "HTTPS_PROXY",
        "https_proxy",
        "NO_PROXY",
        "no_proxy",
        JUDGE_DIR_ENV,
        JUDGE_SCHEMA_ENV,
        "CLAUDE_CODE_OAUTH_TOKEN",
        // What bash sets itself.
        "PWD",
        "OLDPWD",
        "SHLVL",
        "_",
    ];
    for line in env.lines() {
        let name = line.split_once('=').map_or(line, |(n, _)| n);
        assert!(given.contains(&name), "the look was given {name}");
    }
    // Its brief, with every thread and where things are.
    let conary_core::plugin::Task::Job { prompt, .. } = sent.task else {
        panic!("expected Task::Job")
    };
    assert!(prompt.contains("> It ends on the empty line, by design."));
    assert!(prompt.contains(&format!("{}/head", dir.display())));
}

#[test]
fn a_looks_only_way_out_is_its_gate_to_the_engines_api() {
    let r = rig();
    let request = judge_request(&r.w, vec![Trigger::Asked]);
    r.judge(&request).unwrap();
    let proxy: Vec<String> = r.seen("proxy").lines().map(str::to_owned).collect();
    assert_eq!(
        proxy,
        [
            "HTTP/1.1 403 Forbidden",
            "HTTP/1.1 200 Connection established"
        ],
        "{proxy:?}"
    );
    assert_eq!(*r.dialed.lock().unwrap(), ["api.anthropic.com"]);
}

#[test]
fn a_look_that_leaves_no_answer_that_reads_answers_nothing() {
    let r = rig();
    let request = judge_request(&r.w, vec![Trigger::Asked]);
    for (printed, says) in [
        (json!("not a result"), "no response that reads"),
        (
            {
                let mut p = printed("pass", "a-model");
                p["by"]["engine"] = json!("other");
                p
            },
            "answered as",
        ),
        (
            {
                let mut p = printed("pass", "a-model");
                p["by"]["session"] = json!(" ");
                p
            },
            "no session",
        ),
        (
            {
                let mut p = printed("pass", "a-model");
                p["execution"]["context"] = json!({"kind": "continue", "session": "old"});
                p
            },
            "fresh session",
        ),
        (
            {
                let mut p = printed("pass", "a-model");
                p["summary"] = json!({"verdict": "pass", "why": " "}).to_string().into();
                p
            },
            "no reasons",
        ),
        (
            {
                let mut p = printed("pass", "a-model");
                p["summary"] = json!("not json");
                p
            },
            "no valid verdict JSON",
        ),
        (printed("pass", "b-model"), "not a-model"),
        (
            printed_for("claude", "pass", "a-model", Some("low")),
            "ran effort",
        ),
        (printed("land", "a-model"), "no valid verdict JSON"),
    ] {
        r.prints(&printed);
        let ran = r.asked().ran.len();
        let got = r.judge(&request);
        assert!(got.is_err(), "{printed}: {got:?}");
        // Refused for what it left, once it ran.
        assert_eq!(r.asked().ran.len(), ran + 1, "{printed}: {got:?}");
        assert!(got.unwrap_err().contains("the judge's look failed"));
        // What the worker said goes to the unit's log; the producer reads
        // only that the look failed, and nothing it left.
        let spec = Spec {
            engine: "claude".into(),
            repo: REPO.into(),
            prompt: String::new(),
            model: "a-model".into(),
            effort: Some("high".into()),
        };
        let why = looked(printed.to_string().as_bytes(), &spec).unwrap_err();
        assert!(why.contains(says), "{printed}: {why}");
    }
    // Nothing left behind, whichever way it failed.
    assert_eq!(
        std::fs::read_dir(r.dir.path().join("state/looks"))
            .unwrap()
            .count(),
        0
    );
}

#[test]
fn the_producer_takes_only_the_answer_its_look_left() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(ANSWER);
    // Blank, as it's laid out: the look never wrote it.
    std::fs::write(&path, b"").unwrap();
    assert!(Looked::read(&std::fs::read(&path).unwrap()).is_err());
    let ok = Looked {
        engine: "claude".into(),
        verdict: Judgment::Fail,
        why: "It breaks the build.".into(),
        session: "s".into(),
        model: "a-model".into(),
    };
    assert_eq!(Looked::read(&serde_json::to_vec(&ok).unwrap()).unwrap(), ok);
    for (field, blank) in [("engine", " "), ("why", " "), ("session", "")] {
        let mut v = serde_json::to_value(&ok).unwrap();
        v[field] = json!(blank);
        assert!(Looked::read(v.to_string().as_bytes()).is_err(), "{field}");
    }
    let mut v = serde_json::to_value(&ok).unwrap();
    v["extra"] = json!(1);
    assert!(Looked::read(v.to_string().as_bytes()).is_err());
}

#[test]
fn a_head_no_ci_ran_on_or_a_discussion_that_isnt_the_requests_runs_no_look() {
    let r = rig();
    // Its flow names nothing to build it with.
    let got = r.judge(&judge_request(&r.w, vec![Trigger::NoCi, Trigger::Asked]));
    assert!(got.unwrap_err().contains("nothing is judged unbuilt"));
    let request = judge_request(&r.w, vec![Trigger::Asked]);
    r.w.with(|p| p.threads.clear());
    assert!(r.judge(&request).is_err());
    assert!(r.asked().ran.is_empty());
    assert!(!r.dir.path().join("seen/request").exists());
}

#[test]
fn a_restart_stops_the_looks_it_lost_and_clears_their_folders() {
    let r = rig();
    let looks = r.looks();
    let lost = r.dir.path().join("state/looks/0123abcd");
    std::fs::create_dir_all(lost.join("head")).unwrap();
    r.asked().loaded = vec!["0123abcd".into()];
    assert_eq!(looks.reconcile().unwrap(), 1);
    assert_eq!(r.asked().stopped, ["0123abcd"]);
    assert!(!lost.exists());
}

#[test]
fn a_looks_answer_that_quotes_its_login_answers_nothing() {
    let r = rig();
    let request = judge_request(&r.w, vec![Trigger::Asked]);
    let mut p = printed("pass", "a-model");
    p["summary"] = json!({"verdict": "pass", "why": "As asked, here it is: the-look-token."})
        .to_string()
        .into();
    r.prints(&p);
    assert!(
        r.judge(&request)
            .unwrap_err()
            .contains("the judge's look failed")
    );
    assert_eq!(r.asked().ran.len(), 1);
    // Any long enough run of it, not only the whole of it.
    let token = "sk-ant-oat01-ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    assert!(holds_login(&format!("x {token} y"), token).is_some());
    assert!(holds_login("half: KLMNOPQRSTUVWXYZ0123456789", token).is_some());
    assert!(holds_login("it uses sk-ant-oat01- tokens", token).is_none());
    assert!(holds_login("Read the diff.", token).is_none());
}

#[test]
fn a_look_whose_request_ends_is_stopped_and_answers_nothing() {
    let r = rig();
    r.asked().abandon = true;
    let p = r.producer();
    let request = judge_request(&r.w, vec![Trigger::Asked]);
    assert!(ask(r.dir.path(), &p, &request).is_err());
    let asked = r.asked();
    assert_eq!(asked.stopped, asked.ran, "{:?}", asked.stopped);
    assert_eq!(asked.ran.len(), 1);
    assert!(!r.dir.path().join("seen/request").exists());
    assert_eq!(
        std::fs::read_dir(r.dir.path().join("state/looks"))
            .unwrap()
            .count(),
        0
    );
    assert!(r.w.posted().is_empty());
    assert!(r.w.with(|p| p.runs.is_empty()));
}

#[test]
fn a_judgment_a_look_cant_take_is_refused_before_it_burns_a_generation() {
    let r = rig();
    let p = r.producer();
    let got = ask(
        r.dir.path(),
        &p,
        &judge_request(&r.w, vec![Trigger::NoCi, Trigger::Asked]),
    );
    // No CI, and its flow names nothing to build it with.
    assert!(got.unwrap_err().contains("nothing is judged unbuilt"));
    // No generation was allocated, no look ran, nothing was posted.
    assert!(!r.dir.path().join("state/generations").exists());
    assert!(r.asked().ran.is_empty());
    assert!(r.w.posted().is_empty());
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy test")]
fn a_look_dispatches_to_a_routed_non_claude_engine_with_its_own_login_and_egress() {
    let r = rig();
    // Register a second modular engine surface (`custom`) alongside `claude`,
    // with its own credential, credential env var, and wildcard egress hosts.
    let custom_plugin = stand_in_for(
        r.dir.path(),
        "custom",
        &[
            "example.org:443",
            "api.anthropic.com:443",
            "generativelanguage.googleapis.com:443",
        ],
    );
    let custom_cfg = EngineConfig {
        plugin: custom_plugin,
        unit: "autoland-judge-look-custom".into(),
        credential: "judge-login-custom".into(),
        credential_env: "CUSTOM_ENGINE_TOKEN".into(),
        hosts: vec!["*.googleapis.com".into()],
    };
    let custom_units = InProcess {
        engines: BTreeMap::from([("custom".into(), custom_cfg.clone())]),
        credentials: BTreeMap::from([(
            "judge-login-custom".into(),
            b"custom-secret-token\n".to_vec(),
        )]),
        looks: r.units.looks.clone(),
        home: r.units.home.clone(),
        asked: r.units.asked.clone(),
    };
    // Build config first (which seeds default claude overlay), then route
    // `judge` to `custom` via the live settings overlay without changing code.
    let mut cfg = config(r.dir.path());
    cfg.engines.insert("custom".into(), custom_cfg);
    std::fs::write(
        r.dir.path().join("overlay.roles.json"),
        json!({
            "routing": {
                "roles": {"judge": {"engine": "custom"}},
                "models": {"judge custom": {"model": "custom-model", "effort": "medium"}}
            },
            "offers": {"custom": {"models": ["custom-model"], "efforts": ["medium"]}},
            "migrated": 1
        })
        .to_string(),
    )
    .unwrap();
    r.prints(&printed_for(
        "custom",
        "pass",
        "custom-model",
        Some("medium"),
    ));

    let p = crate::plugin(&cfg).unwrap();
    assert_eq!(p.by.engine, "custom");

    let units: BTreeMap<String, Box<dyn Units>> = BTreeMap::from([
        ("claude".into(), Box::new(r.units.clone()) as Box<dyn Units>),
        ("custom".into(), Box::new(custom_units) as Box<dyn Units>),
    ]);
    let looks = Looks::new(
        cfg.clone(),
        read(&r.w),
        units,
        Box::new(r.builds.clone()),
        api(r.dialed.clone()),
    )
    .unwrap();
    let producer = Producer::new(
        cfg,
        read(&r.w),
        github(&r.w),
        Box::new(looks),
        Signer::for_tests(PRODUCER, SECRET),
        seal(),
    )
    .unwrap();
    let request: Request = serde_json::from_value(json!({
        "contract": conary_core::plugin::CONTRACT,
        "execution": {"choice": {"engine": "custom", "model": "custom-model", "effort": "medium"},
            "context": {"kind": "fresh"}},
        "negotiated": {"by": {"engine": "custom", "session": null},
            "offers": {"models": [{"id": "custom-model", "efforts": ["medium"]}],
                "capabilities": {"sandbox": "brings", "network": true, "devices": [], "mcp": false},
                "meters": []}},
        "kind": "judge", "repo": REPO, "pr": super::PR, "head": r.w.with(|p| p.head.clone()),
        "against": r.w.against(), "author": {"engine": "claude", "session": super::AUTHOR},
        "reviewers": [{"engine": "codex", "session": super::REVIEWER}],
        "why": [Trigger::Asked], "context": r.w.context(),
    }))
    .unwrap();

    let got = ask(r.dir.path(), &producer, &request).unwrap();
    assert_eq!(got["verdict"], "pass", "{got}");
    assert_eq!(
        got["by"],
        json!({"engine": "custom", "session": "look-session"})
    );
    assert_eq!(got["execution"]["model"], "custom-model");
    assert_eq!(got["execution"]["effort"], "medium");
    // Only the selected engine's login env was injected (never claude's).
    let env = r.seen("env");
    assert!(
        env.lines()
            .any(|l| l == "CUSTOM_ENGINE_TOKEN=custom-secret-token"),
        "{env}"
    );
    assert!(
        !env.lines()
            .any(|l| l.starts_with("CLAUDE_CODE_OAUTH_TOKEN=")),
        "{env}"
    );
    // Egress matched `*.googleapis.com` alone: example.org and api.anthropic.com rejected.
    let proxy: Vec<String> = r.seen("proxy").lines().map(str::to_owned).collect();
    assert_eq!(
        proxy,
        [
            "HTTP/1.1 403 Forbidden",
            "HTTP/1.1 403 Forbidden",
            "HTTP/1.1 200 Connection established",
        ],
        "{proxy:?}"
    );
    assert_eq!(
        *r.dialed.lock().unwrap(),
        ["generativelanguage.googleapis.com"]
    );
    // Sealed verdict + attested check run published as usual.
    assert_eq!(r.w.posted().len(), 1);
    assert_eq!(r.w.completed().len(), 1);
}
