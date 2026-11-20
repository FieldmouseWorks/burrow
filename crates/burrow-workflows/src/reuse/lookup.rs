//! The lookup's step, with the real runner (compiled by run.sh as on a
//! runner) asking, through the machine's curl, a lookup that keeps the
//! contract: a server of the test's own, on this machine.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use super::{CONTRACT, RUN, identity_env, wf, workflow_repo};
use crate::harness::{Scratch, repo_file, script, step, which};

/// The runner's machine, with a compiler that stands in for compiler.nix's
/// (the gate's tests do the same), so run.sh compiles the real runner.
struct Lookup {
    s: Scratch,
}

/// One request the server was sent: its path and query, its headers (as
/// sent, one per line) and its body.
#[derive(Clone, Debug)]
struct Request {
    path: String,
    headers: String,
    body: Vec<u8>,
}

/// The lookup and the token service, as one server on 127.0.0.1: a pass
/// on record for `evidence` (the repository, the tree and the gate but its
/// commit, which its tree stands for), each request answered by its digest,
/// hit or miss, unless the case says how to answer.
struct Server {
    url: String,
    asked: Arc<Mutex<Vec<Request>>>,
    stop: Arc<AtomicBool>,
}

/// What the lookup is asked to match: the request but its own commit, and
/// the gate's workflow commit.
fn key(body: &[u8]) -> Option<Json> {
    let mut r: Json = serde_json::from_slice(body).ok()?;
    r["gate"]["workflow"].as_object_mut()?.remove("commit");
    Some(json!({"repository": r["repository"], "tree": r["tree"], "gate": r["gate"]}))
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A request off `conn`: its line, headers and body (by Content-Length).
fn read_request(conn: &TcpStream) -> Option<Request> {
    let mut r = BufReader::new(conn.try_clone().ok()?);
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let path = line.split_whitespace().nth(1)?.to_owned();
    let (mut headers, mut length) = (String::new(), 0);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? <= 2 {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().ok()?;
        }
        headers.push_str(h.trim_end());
        headers.push('\n');
    }
    let mut body = vec![0; length];
    r.read_exact(&mut body).ok()?;
    Some(Request {
        path,
        headers,
        body,
    })
}

impl Server {
    fn start(case: &Case) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (asked, stop) = (
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(AtomicBool::new(false)),
        );
        let (log, stopped) = (asked.clone(), stop.clone());
        let (evidence, reply, status, token_fails) = (
            case.evidence.cloned(),
            case.reply.to_owned(),
            case.status,
            case.token_fails,
        );
        std::thread::spawn(move || {
            for mut conn in listener.incoming().flatten() {
                if stopped.load(Ordering::SeqCst) {
                    break;
                }
                let Some(req) = read_request(&conn) else {
                    continue;
                };
                log.lock().unwrap().push(req.clone());
                let (code, body) = if req.path.starts_with("/idtoken") {
                    if token_fails {
                        (403, String::new())
                    } else {
                        (200, r#"{"count": 1, "value": "the-token"}"#.to_owned())
                    }
                } else if status != 200 {
                    (status, String::new())
                } else {
                    let query = hex(&req.body);
                    let answer = if !reply.is_empty() {
                        reply.replace("QUERY", &query)
                    } else if evidence.is_some() && key(&req.body) == evidence {
                        json!({"contract": CONTRACT, "query": query, "evidence": "hit", "run": RUN})
                            .to_string()
                    } else {
                        json!({"contract": CONTRACT, "query": query, "evidence": "miss"})
                            .to_string()
                    };
                    (200, answer)
                };
                let _ = conn.write_all(
                    format!(
                        "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
            }
        });
        Server { url, asked, stop }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.url.trim_start_matches("http://"));
    }
}

/// A URL on this machine that nothing answers.
fn unreachable() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}/gate", l.local_addr().unwrap())
}

impl Lookup {
    fn new() -> Lookup {
        let s = Scratch::default();
        let sysroot = String::from_utf8(
            std::process::Command::new("rustc")
                .args(["--print", "sysroot"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        fs::create_dir_all(s.join("compiler/bin")).unwrap();
        crate::harness::executable(
            &s.join("compiler/bin/rustc"),
            &format!("exec {}/bin/rustc \"$@\"\n", sysroot.trim()),
        );
        std::os::unix::fs::symlink(which("cc"), s.join("compiler/bin/cc")).unwrap();
        let expression = repo_file(".github/flake-check/compiler.nix")
            .canonicalize()
            .unwrap();
        s.stub(
            "nix-build",
            &format!(
                "[ \"$3\" = '{}' ] || exit 90\nprintf '%s\\n' '{}'\n",
                expression.display(),
                s.join("compiler").display()
            ),
        );
        Lookup { s }
    }

    /// Runs the identity step, then the lookup's, for the push the case
    /// names, against a workflow checkout at `at` whose commit is `sha`.
    fn ask(&self, at: &Path, sha: &str, case: &Case) -> Asked {
        let server = Server::start(case);
        let temp = self.s.mkdtemp();
        let mut env = identity_env(case.inputs, sha);
        env.push(("RUNNER_TEMP", temp.to_str().unwrap()));
        let wf = wf();
        let id = self
            .s
            .bash(script(step(&wf["jobs"]["reuse"], "identity")), at, &env);
        assert_eq!(id.code, 0, "{}", id.said());
        let tools = repo_file(".github/flake-check").canonicalize().unwrap();
        let lookup = case
            .lookup
            .map_or_else(|| format!("{}/gate", server.url), str::to_owned);
        let token_url = format!("{}/idtoken?api-version=2.0", server.url);
        let mut env: Vec<(&str, &str)> = vec![
            ("RUNNER_TEMP", temp.to_str().unwrap()),
            ("TOOLS", tools.to_str().unwrap()),
            ("LOOKUP", &lookup),
            ("COMMIT", case.commit),
            ("HEAD_ID", case.head_id.unwrap_or(case.commit)),
            ("TREE", case.tree),
            ("GITHUB_REPOSITORY", "owner/repo"),
            ("GITHUB_REF", case.git_ref),
            ("DEFAULT_BRANCH", "main"),
            ("GITHUB_SERVER_URL", "https://github.com"),
        ];
        if case.oidc {
            env.push(("ACTIONS_ID_TOKEN_REQUEST_URL", &token_url));
            env.push(("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request-token"));
        }
        let r = self
            .s
            .bash(script(step(&wf["jobs"]["reuse"], "lookup")), at, &env);
        let asked = server.asked.lock().unwrap().clone();
        let (token, lookups): (Vec<_>, Vec<_>) = asked
            .into_iter()
            .partition(|r| r.path.starts_with("/idtoken"));
        let last = lookups.last().cloned();
        Asked {
            code: r.code,
            said: r.said(),
            outputs: r.outputs,
            key: last.as_ref().and_then(|r| key(&r.body)),
            body: last
                .as_ref()
                .map(|r| String::from_utf8(r.body.clone()).unwrap()),
            headers: last.map(|r| r.headers),
            token: token.last().cloned(),
            asked: !lookups.is_empty(),
        }
    }
}

/// What a case asks with: a push of the commit whose tree it names, with
/// the inputs it changes, a pass on record for `evidence`, and the lookup
/// answering as it says (`lookup`: another URL than the server's).
#[derive(Clone, Copy)]
struct Case<'a> {
    lookup: Option<&'a str>,
    commit: &'a str,
    head_id: Option<&'a str>,
    tree: &'a str,
    inputs: &'a [(&'a str, &'a str)],
    evidence: Option<&'a Json>,
    reply: &'a str,
    status: u16,
    oidc: bool,
    git_ref: &'a str,
    token_fails: bool,
}

const COMMIT: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";

impl Default for Case<'_> {
    fn default() -> Self {
        Case {
            lookup: None,
            commit: COMMIT,
            head_id: None,
            tree: TREE,
            inputs: &[],
            evidence: None,
            reply: "",
            status: 200,
            oidc: true,
            git_ref: "refs/heads/main",
            token_fails: false,
        }
    }
}

struct Asked {
    code: i32,
    said: String,
    outputs: String,
    key: Option<Json>,
    body: Option<String>,
    headers: Option<String>,
    token: Option<Request>,
    asked: bool,
}

impl Asked {
    fn hit(&self) -> bool {
        self.code == 0 && self.outputs == format!("hit=true\nrun={RUN}\n")
    }

    /// A miss: the gate runs, and nothing failed.
    fn miss(&self) -> bool {
        self.code == 0 && self.outputs == "hit=false\n"
    }

    /// No answer it can trust: the gate runs, and the step fails softly
    /// (degraded/gate-reuse), having said nothing of a hit.
    fn unknown(&self) -> bool {
        self.code != 0 && !self.outputs.contains("hit=")
    }
}

/// A pass on record for exactly this push's tree and gate is a hit; the
/// request goes signed with this job's token, whose audience names the
/// request's digest.
#[test]
fn an_exact_match_is_a_hit() {
    let l = Lookup::new();
    let at = l.s.mkdtemp();
    let sha = workflow_repo(&l.s, &at, "v1");
    // What the lookup is asked for this push: then a pass on record for it.
    let first = l.ask(&at, &sha, &Case::default());
    assert!(first.miss(), "{}", first.said);
    let key = first.key.clone().expect("the lookup was asked");
    let a = l.ask(
        &at,
        &sha,
        &Case {
            evidence: Some(&key),
            ..Case::default()
        },
    );
    assert!(a.hit(), "{}\n{}", a.outputs, a.said);
    let body = a.body.unwrap();
    let request: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(request["contract"], CONTRACT);
    assert_eq!(request["repository"], "owner/repo");
    assert_eq!(request["commit"], COMMIT);
    assert_eq!(request["tree"], TREE);
    assert_eq!(request["gate"]["workflow"]["commit"], sha.as_str());
    assert_eq!(body.lines().count(), 1, "{body}");
    let digest = hex(body.as_bytes());
    let token = a.token.unwrap();
    assert_eq!(
        token.path,
        format!("/idtoken?api-version=2.0&audience=burrow-gate-reuse:v1:sha256:{digest}")
    );
    assert!(
        token
            .headers
            .contains("Authorization: bearer request-token\n"),
        "{}",
        token.headers
    );
    let headers = a.headers.unwrap();
    assert!(
        headers.contains("Authorization: Bearer the-token\n")
            && headers.contains("Content-Type: application/json\n"),
        "the request isn't signed: {headers}"
    );
    // Another commit of the same tree is the same tree: the gate passed on
    // it, whichever commit it was.
    let other = "3333333333333333333333333333333333333333";
    let a = l.ask(
        &at,
        &sha,
        &Case {
            evidence: Some(&key),
            commit: other,
            ..Case::default()
        },
    );
    assert!(a.hit(), "{}", a.said);
    // So is another commit of the workflow with the same files: its tree
    // stands for it.
    let same = {
        let repo = at.join(".flake-check");
        l.s.git(&repo, &["commit", "-q", "--allow-empty", "-m", "again"]);
        l.s.git(&repo, &["rev-parse", "HEAD"])
    };
    assert_ne!(same, sha);
    let a = l.ask(
        &at,
        &same,
        &Case {
            evidence: Some(&key),
            ..Case::default()
        },
    );
    assert!(a.hit(), "{}", a.said);
}

/// Anything but that exact tree and gate is a miss, and the whole gate
/// runs: another tree, another workflow (its files changed), or any input
/// that can change the result.
#[test]
fn another_tree_workflow_or_input_misses() {
    let l = Lookup::new();
    let at = l.s.mkdtemp();
    let sha = workflow_repo(&l.s, &at, "v1");
    let key = l.ask(&at, &sha, &Case::default()).key.unwrap();
    let evidence = Case {
        evidence: Some(&key),
        ..Case::default()
    };
    assert!(l.ask(&at, &sha, &evidence).hit());
    let tree = "4444444444444444444444444444444444444444";
    let a = l.ask(&at, &sha, &Case { tree, ..evidence });
    assert!(a.miss(), "{}", a.said);
    for over in [
        ("RUNNER_LABEL", "remi"),
        ("INSTALL_NIX", "false"),
        ("APART", "false"),
        ("LEGS", "1"),
        ("SKIP", ""),
        ("SKIP", "agents vm"),
        ("KEEP", "false"),
        ("WORKFLOW_FILE", ".github/workflows/other.yml"),
        ("WORKFLOW_REPOSITORY", "someone/burrow"),
    ] {
        let inputs = [over];
        let a = l.ask(
            &at,
            &sha,
            &Case {
                evidence: Some(&key),
                inputs: &inputs,
                ..Case::default()
            },
        );
        assert!(a.miss(), "{over:?}: {}", a.said);
    } // The workflow's files changed: another gate.
    let changed = workflow_repo(&l.s, &at, "v2");
    let a = l.ask(
        &at,
        &changed,
        &Case {
            evidence: Some(&key),
            ..Case::default()
        },
    );
    assert!(a.miss(), "{}", a.said);
}

/// A lookup that can't be reached, answers with an error, can't tell, or
/// answers anything the runner can't hold to this request, is no hit: the
/// gate runs, and the step fails softly. So does a push the event names no
/// tree for, a lookup that isn't https (or this machine's), and a token
/// the job can't get.
#[test]
fn errors_and_malformed_answers_run_the_gate() {
    let l = Lookup::new();
    let at = l.s.mkdtemp();
    let sha = workflow_repo(&l.s, &at, "v1");
    let key = l.ask(&at, &sha, &Case::default()).key.unwrap();
    let hit = |reply: &str| {
        format!(
            "{{\"contract\": \"{CONTRACT}\", \"query\": \"QUERY\", \"evidence\": \"hit\", \"run\": \"{reply}\"}}"
        )
    };
    let foreign = hit("https://github.com/someone/repo/actions/runs/41");
    let not_a_run = hit("https://github.com/owner/repo/pulls/41");
    let wrong_query = hit(RUN).replace("QUERY", &"0".repeat(64));
    let no_contract = hit(RUN).replace(CONTRACT, "burrow-gate-reuse/v0");
    let shouted = hit(RUN).replace("\"hit\"", "\"HIT\"");
    let unknown = hit(RUN).replace("\"hit\"", "\"unknown\"");
    let twice = format!("{} {}", hit(RUN), hit(RUN));
    let nobody = unreachable();
    for (what, case) in [
        (
            "unreachable",
            Case {
                lookup: Some(&nobody),
                ..Case::default()
            },
        ),
        (
            "HTTP 500",
            Case {
                status: 500,
                ..Case::default()
            },
        ),
        (
            "HTTP 404",
            Case {
                status: 404,
                ..Case::default()
            },
        ),
        (
            "not JSON",
            Case {
                reply: "<html>hit</html>",
                ..Case::default()
            },
        ),
        (
            "empty",
            Case {
                reply: " ",
                ..Case::default()
            },
        ),
        (
            "an array",
            Case {
                reply: "[\"hit\"]",
                ..Case::default()
            },
        ),
        (
            "another repo's run",
            Case {
                reply: &foreign,
                ..Case::default()
            },
        ),
        (
            "not a run",
            Case {
                reply: &not_a_run,
                ..Case::default()
            },
        ),
        (
            "another request",
            Case {
                reply: &wrong_query,
                ..Case::default()
            },
        ),
        (
            "another contract",
            Case {
                reply: &no_contract,
                ..Case::default()
            },
        ),
        (
            "not hit",
            Case {
                reply: &shouted,
                ..Case::default()
            },
        ),
        (
            "can't tell",
            Case {
                reply: &unknown,
                ..Case::default()
            },
        ),
        (
            "two answers",
            Case {
                reply: &twice,
                ..Case::default()
            },
        ),
        (
            "no token",
            Case {
                token_fails: true,
                ..Case::default()
            },
        ),
    ] {
        let a = l.ask(
            &at,
            &sha,
            &Case {
                evidence: Some(&key),
                ..case
            },
        );
        assert!(a.unknown(), "{what}: {}\n{}", a.outputs, a.said);
        assert!(a.said.contains("so the gate runs"), "{what}: {}", a.said);
    }
    // Asks nothing of a lookup it can't trust, or for a push with no tree.
    for (what, case) in [
        (
            "no lookup",
            Case {
                lookup: Some(""),
                ..Case::default()
            },
        ),
        (
            "plain http",
            Case {
                lookup: Some("http://lookup.example/gate"),
                ..Case::default()
            },
        ),
        (
            "no tree",
            Case {
                tree: "",
                ..Case::default()
            },
        ),
        (
            "another commit's tree",
            Case {
                head_id: Some("5555555555555555555555555555555555555555"),
                ..Case::default()
            },
        ),
        // No token to sign with: a hit could come from anyone.
        (
            "no OIDC in the job",
            Case {
                oidc: false,
                ..Case::default()
            },
        ),
    ] {
        let a = l.ask(
            &at,
            &sha,
            &Case {
                evidence: Some(&key),
                ..case
            },
        );
        assert!(a.unknown(), "{what}: {}\n{}", a.outputs, a.said);
        assert!(!a.asked, "{what}: it asked");
    }
    // (This machine's own lookup may be plain http: every hit here is
    // one.)
    // A remote lookup over plain http, dressed as this machine's: asks
    // nothing.
    let a = l.ask(
        &at,
        &sha,
        &Case {
            evidence: Some(&key),
            lookup: Some("http://localhost:80@lookup.example/gate"),
            ..Case::default()
        },
    );
    assert!(a.unknown() && !a.asked, "{}", a.said);
}

/// A push to a branch named as the default branch but for its case is
/// another branch: it asks nothing and builds, as without gate-reuse, and
/// nothing failed.
#[test]
fn only_the_default_branch_by_its_exact_name_asks() {
    let l = Lookup::new();
    let at = l.s.mkdtemp();
    let sha = workflow_repo(&l.s, &at, "v1");
    let key = l.ask(&at, &sha, &Case::default()).key.unwrap();
    for git_ref in [
        "refs/heads/Main",
        "refs/heads/MAIN",
        "refs/heads/main2",
        "refs/tags/main",
    ] {
        let a = l.ask(
            &at,
            &sha,
            &Case {
                evidence: Some(&key),
                git_ref,
                ..Case::default()
            },
        );
        assert!(a.miss() && !a.asked, "{git_ref}: {}", a.said);
    }
}
