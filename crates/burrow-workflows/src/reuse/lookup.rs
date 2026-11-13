//! The lookup's step, with the real runner (compiled by run.sh as on a
//! runner) and a stand-in for the lookup that keeps the contract.

use std::fs;
use std::path::{Path, PathBuf};

use super::{CONTRACT, RUN, identity_env, wf, workflow_repo};
use crate::harness::{Scratch, repo_file, script, step, which};

/// A stand-in lookup that keeps the contract: a pass is on record for one
/// key (the repository, the tree and the gate but its commit, which its
/// tree stands for), and it answers each request, by its digest, hit or
/// miss; or as the case says. It also stands in for the token service.
struct Lookup {
    s: Scratch,
    log: PathBuf,
}

impl Lookup {
    fn new() -> Lookup {
        let s = Scratch::default();
        // The runner, compiled by run.sh as on a runner, by a compiler that
        // stands in for compiler.nix's (the gate's tests do the same).
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
        let log = s.join("curl");
        s.stub(
            "curl",
            r#"for url; do :; done
case $url in
  *idtoken*)
    printf '%s\n' "$@" >> "$CURL_LOG.token"
    [ -z "$TOKEN_FAIL" ] || exit 22
    echo '{"count": 1, "value": "the-token"}'
    exit 0 ;;
esac
prev=
for a; do
  case $prev in --data-binary) body=${a#@} ;; -o) out=$a ;; -H) headers=${a#@} ;; esac
  prev=$a
done
echo "$url" >> "$CURL_LOG.urls"
cat "$headers" >> "$CURL_LOG.headers"
cp "$body" "$CURL_LOG.body"
[ -z "$CURL_EXIT" ] || { echo 'curl: (7) Failed to connect' >&2; exit "$CURL_EXIT"; }
query=$(sha256sum < "$body" | cut -d' ' -f1)
key=$(jq -cS '{repository, tree, gate: (.gate | del(.workflow.commit))}' "$body")
echo "$key" > "$CURL_LOG.key"
if [ -n "$REPLY" ]; then
  reply=${REPLY//QUERY/$query}
elif [ "$key" = "$EVIDENCE" ]; then
  reply="{\"contract\": \"burrow-gate-reuse/v1\", \"query\": \"$query\", \"evidence\": \"hit\", \"run\": \"$RUN\"}"
else
  reply="{\"contract\": \"burrow-gate-reuse/v1\", \"query\": \"$query\", \"evidence\": \"miss\"}"
fi
printf '%s' "$reply" > "$out"
printf '%s' "${STATUS:-200}"
"#,
        );
        Lookup { s, log }
    }

    /// Runs the identity step, then the lookup's, for a push of `commit`
    /// whose tree is `tree`, against a workflow checkout at `at`.
    fn ask(&self, at: &Path, sha: &str, case: &Case) -> Asked {
        let temp = self.s.mkdtemp();
        let mut env = identity_env(case.inputs, sha);
        env.push(("RUNNER_TEMP", temp.to_str().unwrap()));
        let wf = wf();
        let id = self
            .s
            .bash(script(step(&wf["jobs"]["reuse"], "identity")), at, &env);
        assert_eq!(id.code, 0, "{}", id.said());
        for f in ["token", "urls", "headers", "body", "key"] {
            let _ = fs::remove_file(self.log.with_extension(f));
        }
        let tools = repo_file(".github/flake-check").canonicalize().unwrap();
        let log = self.log.to_str().unwrap();
        let mut env: Vec<(&str, &str)> = vec![
            ("RUNNER_TEMP", temp.to_str().unwrap()),
            ("TOOLS", tools.to_str().unwrap()),
            ("LOOKUP", case.lookup),
            ("COMMIT", case.commit),
            ("HEAD_ID", case.head_id.unwrap_or(case.commit)),
            ("TREE", case.tree),
            ("GITHUB_REPOSITORY", "owner/repo"),
            ("GITHUB_REF", case.git_ref),
            ("DEFAULT_BRANCH", "main"),
            ("GITHUB_SERVER_URL", "https://github.com"),
            ("CURL_LOG", log),
            ("EVIDENCE", case.evidence),
            ("RUN", case.run),
            ("REPLY", case.reply),
            ("STATUS", case.status),
            ("CURL_EXIT", case.curl_exit),
            ("TOKEN_FAIL", if case.token_fails { "1" } else { "" }),
        ];
        if case.oidc {
            env.push((
                "ACTIONS_ID_TOKEN_REQUEST_URL",
                "https://token.example/idtoken?api-version=2.0",
            ));
            env.push(("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request-token"));
        }
        let r = self
            .s
            .bash(script(step(&wf["jobs"]["reuse"], "lookup")), at, &env);
        let file = |f: &str| fs::read_to_string(self.log.with_extension(f)).ok();
        Asked {
            code: r.code,
            said: r.said(),
            outputs: r.outputs,
            key: file("key").map(|k| k.trim().to_owned()),
            body: file("body"),
            headers: file("headers"),
            token: file("token"),
            asked: file("urls").is_some(),
        }
    }
}

/// What a case asks with: a push of the commit whose tree it names, with
/// the inputs it changes, a pass on record for `evidence`.
#[derive(Clone, Copy)]
struct Case<'a> {
    lookup: &'a str,
    commit: &'a str,
    head_id: Option<&'a str>,
    tree: &'a str,
    inputs: &'a [(&'a str, &'a str)],
    evidence: &'a str,
    run: &'a str,
    reply: &'a str,
    status: &'a str,
    curl_exit: &'a str,
    oidc: bool,
    git_ref: &'a str,
    token_fails: bool,
}

const COMMIT: &str = "1111111111111111111111111111111111111111";
const TREE: &str = "2222222222222222222222222222222222222222";

impl Default for Case<'_> {
    fn default() -> Self {
        Case {
            lookup: "https://lookup.example/gate",
            commit: COMMIT,
            head_id: None,
            tree: TREE,
            inputs: &[],
            evidence: "",
            run: RUN,
            reply: "",
            status: "200",
            curl_exit: "",
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
    key: Option<String>,
    body: Option<String>,
    headers: Option<String>,
    token: Option<String>,
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
            evidence: &key,
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
    let digest: String = {
        use sha2::Digest;
        sha2::Sha256::digest(body.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    let token = a.token.unwrap();
    assert!(
        token.ends_with(&format!(
            "https://token.example/idtoken?api-version=2.0&audience=burrow-gate-reuse:v1:sha256:{digest}\n"
        )),
        "{token}"
    );
    // Neither token is on a command line.
    assert!(!token.contains("request-token"), "{token}");
    assert!(
        a.headers
            .unwrap()
            .contains("Authorization: Bearer the-token\n"),
        "the request isn't signed"
    );
    // Another commit of the same tree is the same tree: the gate passed on
    // it, whichever commit it was.
    let other = "3333333333333333333333333333333333333333";
    let a = l.ask(
        &at,
        &sha,
        &Case {
            evidence: &key,
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
            evidence: &key,
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
        evidence: &key,
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
                evidence: &key,
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
            evidence: &key,
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
    for (what, case) in [
        (
            "unreachable",
            Case {
                curl_exit: "7",
                ..Case::default()
            },
        ),
        (
            "HTTP 500",
            Case {
                status: "500",
                ..Case::default()
            },
        ),
        (
            "HTTP 404",
            Case {
                status: "404",
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
                evidence: &key,
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
                lookup: "",
                ..Case::default()
            },
        ),
        (
            "plain http",
            Case {
                lookup: "http://lookup.example/gate",
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
                evidence: &key,
                ..case
            },
        );
        assert!(a.unknown(), "{what}: {}\n{}", a.outputs, a.said);
        assert!(!a.asked, "{what}: it asked");
    }
    // This machine's own lookup may be plain http.
    let a = l.ask(
        &at,
        &sha,
        &Case {
            evidence: &key,
            lookup: "http://127.0.0.1:8080/gate",
            ..Case::default()
        },
    );
    assert!(a.hit(), "{}", a.said);
    // A remote lookup over plain http, dressed as this machine's: asks
    // nothing.
    let a = l.ask(
        &at,
        &sha,
        &Case {
            evidence: &key,
            lookup: "http://localhost:80@lookup.example/gate",
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
                evidence: &key,
                git_ref,
                ..Case::default()
            },
        );
        assert!(a.miss() && !a.asked, "{git_ref}: {}", a.said);
    }
}
