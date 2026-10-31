//! A look as Codex takes it: read-only in the snapshot, its commands kept
//! out of its home (its login), the checkout's AGENTS.md off, its effort,
//! its answer held to the schema and read from its last message, its brief
//! the steward's, whole. Out through the gate to its API's hosts, and
//! nowhere else; in a sandbox that shows it its own snapshot and home, and
//! its closure of the store.

use std::collections::BTreeSet;

use burrow_review::codex::{CHECKOUT, schema};
use serde_json::{Value, json};

use crate::rig::{HEAD, Rig, STORE};

/// What it's run with, what it reaches and what comes back.
#[test]
fn codex_reviews_its_snapshot_through_the_gate() {
    let rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    rig.agent(json!({
        "answer": {"findings": [], "summary": "ok"},
        "http": [{"env": "HTTPS_PROXY", "send": "CONNECT chatgpt.com:443 HTTP/1.1\r\n\r\nhello"},
                 {"env": "HTTPS_PROXY", "send": "CONNECT example.com:443 HTTP/1.1\r\n\r\n"},
                 {"env": "HTTPS_PROXY", "send": "CONNECT chatgpt.com:80 HTTP/1.1\r\n\r\n"},
                 {"env": "HTTPS_PROXY", "send": "POST /v1/messages HTTP/1.1\r\nContent-Length: 0\r\n\r\n"}],
    }));
    let entry = rig.queue(&snap, 5, HEAD, json!({}));
    rig.work();
    assert!(rig.names("queue").is_empty());
    let back = rig.back(&entry);
    assert_eq!(
        back,
        json!({"result": "review", "answer": {"findings": [], "summary": "ok"}, "description": "d3scr1pt10n",
               "at": back["at"], "head": HEAD, "base": "main"})
    );
    assert!(back["at"].as_f64().unwrap() > 1.7e9, "{back}");

    let runs = rig.runs();
    let [run] = &runs[..] else { panic!("{runs:?}") };
    let a: Vec<&str> = run["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(&a[..2], ["exec", "--cd"]);
    assert_eq!(a[2], CHECKOUT);
    assert!(
        a.contains(&"--json") && !a.contains(&"--sandbox") && !a.contains(&"--ephemeral"),
        "{a:?}"
    );
    assert_eq!(run["cwd"], rig.path("trees").join(&snap).to_str().unwrap());
    assert_eq!(run["resumed"], false);
    assert_eq!(run["prompt"], "You review pull request #5 in sam/r.");
    let home = rig.session(&snap).join("home");
    let login = rig.home().join(".codex");
    for c in [
        r#"default_permissions="review""#.to_owned(),
        format!(
            r#"permissions.review.filesystem={{":root"="read", "{}"="none", "{}"="none"}}"#,
            home.display(),
            login.display()
        ),
        "project_doc_max_bytes=0".to_owned(),
        "model_reasoning_effort=high".to_owned(),
    ] {
        assert!(a.windows(2).any(|w| w == ["-c", c.as_str()]), "{c}: {a:?}");
    }
    let schema_at = a.iter().position(|x| *x == "--output-schema").unwrap();
    assert!(a[schema_at + 1].ends_with("/schema.json"), "{a:?}");
    // The session stays for the PR's next look, which resumes it.
    assert_eq!(rig.look(&snap), json!({"id": run["session"], "head": HEAD}));

    // Its environment is what it needs and no more: no GitHub token, none
    // of the service's own.
    let env = run["env"].as_object().unwrap();
    assert_eq!(env["CODEX_HOME"], home.to_str().unwrap());
    let theirs: BTreeSet<&str> = env
        .keys()
        .map(String::as_str)
        .filter(|k| !["PWD", "SHLVL", "_", "OLDPWD"].contains(k))
        .collect();
    assert_eq!(
        theirs,
        BTreeSet::from([
            "CODEX_HOME",
            "HOME",
            "HTTPS_PROXY",
            "LANG",
            "PATH",
            "SHELL",
            "SSL_CERT_FILE"
        ])
    );
    let proxy = env["HTTPS_PROXY"].as_str().unwrap();
    let port = proxy.strip_prefix("http://127.0.0.1:").unwrap();
    assert!(
        !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()),
        "{proxy}"
    );

    // Out to its API's hosts on 443, and nowhere else.
    let said = rig.http_said();
    assert!(
        said[0].starts_with("HTTP/1.1 200 Connection established\r\n\r\n")
            && said[0].ends_with("ok"),
        "{said:?}"
    );
    let firsts: Vec<&str> = said[1..]
        .iter()
        .map(|x| x.split("\r\n").next().unwrap())
        .collect();
    assert_eq!(firsts, ["HTTP/1.1 403 Forbidden"; 3]);
    assert_eq!(
        *rig.upstream.lock().unwrap(),
        [("chatgpt.com".to_owned(), "hello".to_owned())]
    );
}

/// In a sandbox that shows it its own snapshot and home and its closure of
/// the store: not the other snapshots, the queue, the answers or the
/// reviewers' own home. No network of its own: the gate is its only way out.
#[test]
fn its_sandbox_shows_it_its_snapshot_and_little_else() {
    let rig = Rig::new();
    let snap = rig.snapshot(5, HEAD);
    rig.queue(&snap, 5, HEAD, json!({}));
    rig.work();
    let boxes = rig.boxes();
    let [b] = &boxes[..] else { panic!("{boxes:?}") };
    let args: Vec<&str> = b["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(
        args.contains(&"--unshare-all") && args.contains(&"--die-with-parent"),
        "{args:?}"
    );
    assert!(!args.contains(&"--share-net"), "{args:?}");
    let binds: Vec<(String, String, String)> = b["binds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| (s(&x[0]), s(&x[1]), s(&x[2])))
        .collect();
    let has = |k: &str, src: &str, d: &str| binds.contains(&(k.into(), src.into(), d.into()));
    let snap_dir = rig.path("trees").join(&snap);
    let home = rig.session(&snap).join("home");
    assert!(
        has("--ro-bind", snap_dir.to_str().unwrap(), CHECKOUT),
        "{binds:?}"
    );
    assert!(
        has("--bind", home.to_str().unwrap(), home.to_str().unwrap()),
        "{binds:?}"
    );
    // Of the store, only what it runs on: the whole store holds every
    // flake's sources this machine has read, private repos' included.
    assert!(STORE.iter().all(|p| has("--ro-bind", p, p)), "{binds:?}");
    for d in ["/nix/store", "/nix", "/etc", "/run", "/run/current-system"] {
        assert!(!binds.iter().any(|(_, _, to)| to == d), "{d}: {binds:?}");
    }
    // The machine's own /bin/sh and /usr/bin/env, as links into that closure.
    let links: BTreeSet<(&str, &str)> = args
        .windows(3)
        .filter(|w| w[0] == "--symlink")
        .map(|w| (w[2], w[1]))
        .collect();
    assert_eq!(
        links,
        BTreeSet::from([
            ("/bin/sh", "/nix/store/dddd-bash/bin/sh"),
            ("/usr/bin/env", "/nix/store/eeee-coreutils/bin/env")
        ])
    );
    // Nothing of the service's state but its own snapshot and session, and
    // nothing of the reviewers' home but the git config made for it.
    let reviewers = rig.home();
    for (_, src, to) in &binds {
        let state = ["queue", "done", "sessions", "trees"]
            .iter()
            .any(|d| src.starts_with(rig.path(d).to_str().unwrap()));
        assert!(
            !state || *src == snap_dir.to_str().unwrap() || *src == home.to_str().unwrap(),
            "{src}"
        );
        assert!(*to != reviewers.to_str().unwrap(), "{to}");
        assert!(
            !to.starts_with(&format!("{}/", reviewers.display())) || to.ends_with("/.gitconfig"),
            "{to}"
        );
    }
    // git trusts that one snapshot, which isn't the reviewers' own.
    assert_eq!(
        b["gitconfig"],
        json!([format!("[safe]\n\tdirectory = {CHECKOUT}\n")])
    );
}

fn s(v: &Value) -> String {
    v.as_str().unwrap().to_owned()
}

/// All of each finding: what a complete fix has to hold, which Codex's
/// answer can't leave out.
#[test]
fn every_finding_says_what_a_whole_fix_covers() {
    let schema = schema();
    let required = &schema["properties"]["findings"]["items"]["required"];
    assert!(
        required.as_array().unwrap().contains(&json!("covers")),
        "{required}"
    );
}
