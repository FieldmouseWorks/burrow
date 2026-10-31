//! What the service's tests share: its dirs in a temp dir, a snapshot, the
//! stand-ins for codex and bubblewrap (tests/fakes), the APIs past the gate,
//! and burrow-events' kick port, each recording what it was asked.

use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use burrow_review::config::Config;
use burrow_review::egress::Dial;
use burrow_review::{Service, review, work};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The store paths a look's sandbox shows: its closure, never the whole store.
pub const STORE: [&str; 3] = [
    "/nix/store/aaaa-review-agents",
    "/nix/store/bbbb-python3",
    "/nix/store/cccc-ripgrep",
];
pub const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

/// The stand-ins, written once for every test: a file being written while
/// another test starts a process could be held open in that process as it
/// runs (ETXTBSY), and none is written after the first test starts one.
fn fakes() -> &'static Path {
    static DIR: OnceLock<TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let bash = which("bash");
        for name in ["codex", "bwrap"] {
            let body = fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fakes/{name}.bash")),
            )
            .unwrap();
            let path = dir.path().join(name);
            fs::write(&path, format!("#!{}\n{body}", bash.display())).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        dir
    })
    .path()
}

pub fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} isn't on PATH"))
}

/// Who asked the API past the gate for what: the host, and the bytes that
/// reached it.
pub type Upstream = Arc<Mutex<Vec<(String, String)>>>;

pub struct Rig {
    pub tmp: TempDir,
    pub cfg: Value,
    pub journal: Arc<Mutex<Vec<String>>>,
    pub upstream: Upstream,
    pub kicks: Arc<Mutex<Vec<String>>>,
}

impl Rig {
    pub fn new() -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let t = tmp.path();
        for d in [
            "queue",
            "done",
            "sessions",
            "trees",
            "home/.fake",
            "home/.codex",
            "agents",
        ] {
            fs::create_dir_all(t.join(d)).unwrap();
        }
        fs::copy(fakes().join("codex"), t.join("agents/codex")).unwrap();
        fs::write(t.join("store-paths"), STORE.join("\n") + "\n").unwrap();
        fs::write(t.join("ca.crt"), "a CA\n").unwrap();
        let kicks = Arc::new(Mutex::new(Vec::new()));
        let port = kick_port(kicks.clone());
        let p = |x: &str| t.join(x).to_string_lossy().into_owned();
        let cfg = json!({
            "queue": p("queue"), "done": p("done"), "sessions": p("sessions"), "snapshots": p("trees"),
            "reviewers": {"codex": {"enable": true, "effort": "high", "parallel": 2, "off": []}},
            "timeoutMinutes": 30, "agents": p("agents"),
            "sandbox": fakes().join("bwrap"),
            "box": {"store": p("store-paths"), "caBundle": p("ca.crt"),
                    "links": {"/bin/sh": "/nix/store/dddd-bash/bin/sh", "/usr/bin/env": "/nix/store/eeee-coreutils/bin/env"}},
            "leadRepos": ["Sam/Lead"], "pipelineRepos": ["Sam/Pipe"],
            "kick": format!("http://127.0.0.1:{port}/kick/autoland"),
        });
        let rig = Rig {
            tmp,
            cfg,
            journal: Arc::new(Mutex::new(Vec::new())),
            upstream: Arc::new(Mutex::new(Vec::new())),
            kicks,
        };
        rig.agent(json!({}));
        rig
    }

    pub fn path(&self, x: &str) -> PathBuf {
        self.tmp.path().join(x)
    }

    pub fn home(&self) -> PathBuf {
        self.path("home")
    }

    /// The service, as its config is now.
    pub fn service(&self) -> Arc<Service> {
        let cfg = Config::parse(&self.cfg.to_string()).unwrap();
        let journal = self.journal.clone();
        Arc::new(Service::new(
            cfg,
            PathBuf::from(env!("CARGO_BIN_EXE_burrow-review")),
            self.home(),
            std::env::var_os("PATH").unwrap_or_default(),
            upstream(self.upstream.clone()),
            Arc::new(move |line: &str| journal.lock().unwrap().push(line.to_owned())),
        ))
    }

    pub fn work(&self) {
        work(self.service());
    }

    pub fn review(&self, name: &str) {
        review(&self.service(), name);
    }

    /// A snapshot of PR n's head on main, under the name the steward gives it.
    pub fn snapshot(&self, n: u64, head: &str) -> String {
        let name = format!("pr-sam_r-{n}-{}-1234abcd", &head[..12]);
        fs::create_dir_all(self.path("trees").join(&name)).unwrap();
        fs::write(
            self.path("trees").join(&name).join("app.py"),
            "print('hi')\n",
        )
        .unwrap();
        name
    }

    /// A look queued as the steward queues it, at the snapshot `snap`: its
    /// entry's name.
    pub fn queue(&self, snap: &str, number: u64, head: &str, extra: Value) -> String {
        let mut item = json!({
            "reviewer": "codex", "snap": snap, "repo": "sam/r", "number": number, "headRefOid": head,
            "baseRefName": "main", "prompt": format!("You review pull request #{number} in sam/r."),
            "description": "d3scr1pt10n", "opened": 1.0, "at": 2.0,
        });
        for (k, v) in extra.as_object().unwrap() {
            item[k] = v.clone();
        }
        let name = format!("{}--{snap}.json", item["reviewer"].as_str().unwrap());
        fs::write(self.path("queue").join(&name), item.to_string()).unwrap();
        name
    }

    /// What the stand-in codex does from now on, and a log that starts over.
    pub fn agent(&self, todo: Value) {
        fs::write(self.home().join(".fake/codex.json"), todo.to_string()).unwrap();
        fs::write(self.home().join(".fake/codex.log"), "").unwrap();
    }

    pub fn runs(&self) -> Vec<Value> {
        lines(&self.home().join(".fake/codex.log"), false)
    }

    /// How the sandboxes were set up, since last asked.
    pub fn boxes(&self) -> Vec<Value> {
        lines(&self.home().join(".fake/box.log"), true)
    }

    /// What came back to the agents' requests, since last asked.
    pub fn http_said(&self) -> Vec<String> {
        lines(&self.home().join(".fake/http.log"), true)
            .into_iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    }

    /// The answer left for an entry, taken out of done/ so the next look's
    /// is the only one there.
    pub fn back(&self, name: &str) -> Value {
        let path = self.path("done").join(name);
        let v: Value =
            serde_json::from_slice(&fs::read(&path).unwrap_or_else(|e| panic!("{name}: {e}")))
                .unwrap();
        fs::remove_file(path).unwrap();
        v
    }

    pub fn names(&self, dir: &str) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(self.path(dir))
            .unwrap()
            .map(|x| x.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    pub fn session(&self, snap: &str) -> PathBuf {
        let (pr, base) = (
            snap.rsplitn(3, '-').nth(2).unwrap(),
            snap.rsplit('-').next().unwrap(),
        );
        self.path("sessions").join(format!("codex--{pr}-{base}"))
    }

    pub fn look(&self, snap: &str) -> Value {
        serde_json::from_slice(&fs::read(self.session(snap).join("look.json")).unwrap()).unwrap()
    }

    pub fn said(&self) -> String {
        self.journal.lock().unwrap().join("\n")
    }
}

fn lines(path: &Path, clear: bool) -> Vec<Value> {
    let text = fs::read_to_string(path).unwrap_or_default();
    if clear {
        let _ = fs::write(path, "");
    }
    text.lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The APIs past the gate: each connection's bytes recorded, for half a
/// second of quiet, and a short answer.
fn upstream(seen: Upstream) -> Dial {
    Arc::new(move |host: &str| {
        let l = TcpListener::bind("127.0.0.1:0")?;
        let ours = TcpStream::connect(l.local_addr()?)?;
        let (mut theirs, _) = l.accept()?;
        let (seen, host) = (seen.clone(), host.to_owned());
        thread::spawn(move || {
            theirs
                .set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            let mut got = Vec::new();
            let mut buf = [0; 4096];
            while let Ok(n @ 1..) = theirs.read(&mut buf) {
                got.extend_from_slice(&buf[..n]);
            }
            seen.lock()
                .unwrap()
                .push((host, String::from_utf8_lossy(&got).into_owned()));
            let _ = theirs.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        });
        Ok(ours)
    })
}

/// burrow-events' kick port: which passes were kicked, by path.
fn kick_port(kicks: Arc<Mutex<Vec<String>>>) -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            let mut buf = [0; 4096];
            let n = c.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]);
            if let Some(path) = head.split(' ').nth(1) {
                kicks.lock().unwrap().push(path.to_owned());
            }
            let _ = c.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
        }
    });
    port
}
