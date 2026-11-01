//! The reviewers' snapshot of a head, taken from a real repo standing in for
//! GitHub: what they read, and what they never see.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;
use crate::config::Config;
use crate::testing::{config, put, sh};

/// A path with bytes that aren't UTF-8.
fn raw(bytes: &[u8]) -> PathBuf {
    PathBuf::from(OsString::from_vec(bytes.to_vec()))
}

const UTIL: &str = "def u():\n    x0 = 0\n    x1 = 1\n    x2 = 2\n    x3 = 3\n    x4 = 4\n    x5 = 5\n    x6 = 6\n    x7 = 7\n    x8 = 8\n    x9 = 9\n    x10 = 10\n    return ";

/// GitHub, as a bare repo at the config's `git`: main, and PR 5's head on
/// refs/pull/5/head, which main has moved on from since it branched.
struct Rig {
    _d: tempfile::TempDir,
    cfg: Config,
    work: PathBuf,
    head: String,
}

fn rig() -> Rig {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(d.path());
    cfg.snapshot.skip_paths = [
        "(^|/)secrets?/",
        r"(^|/)\.sops\.yaml$",
        r"\.sops\.(ya?ml|json|env)$",
        r"(^|/)\.env(\..*)?$",
        r"\.(pem|key|p12|pfx)$",
    ]
    .map(String::from)
    .to_vec();
    // What this repo keeps private on top of the machine's list: a game
    // image and its folder, hidden here and nowhere else.
    // and its folder, a folder matched only with its slash, and one
    // folder's AGENTS.md, all hidden here and nowhere else.
    cfg.snapshot.repos = BTreeMap::from([(
        "sam/r".into(),
        [r"(^|/)roms/", r"\.gen$", r"^vault/$", r"^tools/AGENTS\.md$"]
            .map(String::from)
            .to_vec(),
    )]);
    fs::create_dir_all(cfg.state_dir.join("review/snap")).unwrap();
    fs::create_dir_all(&cfg.snapshot.dir).unwrap();
    let origin = Path::new(&cfg.git).join("sam/r.git");
    fs::create_dir_all(&origin).unwrap();
    sh(&origin, &["init", "-q", "--bare"]);
    let work = d.path().join("work");
    fs::create_dir_all(&work).unwrap();
    sh(&work, &["init", "-q", "-b", "main"]);
    let w = &work;
    put(w, "src/app.py", "def f():\n    a = 1\n    return a\n");
    put(
        w,
        "AGENTS.md",
        "# r\n\n## House rules\n\n- old house rule\n\n## Code Review Rules\n\n- root rule\n",
    );
    put(
        w,
        "src/AGENTS.md",
        "# x\n\n## Code Review Rules\n\n- never call eval\n\n### Within\n\n- still src's\n\n## House rules\n\n- src's house rule\n\n## Other\nnot rules\n",
    );
    put(
        w,
        "docs/AGENTS.md",
        "## House rules\n\n- docs' house rule\n",
    );
    put(
        w,
        "lib/AGENTS.md",
        "## House rules\n\n- lib's house rule\n\n## Code Review Rules\n\n- lib's review rule\n",
    );
    // Where a secret is moved to below: what it says isn't the brief's,
    // since no reviewer is shown what changed there.
    put(
        w,
        "config/AGENTS.md",
        "## House rules\n\n- config's house rule\n",
    );
    put(
        w,
        "lib/core/AGENTS.md",
        "## Code Review Rules\n\n- core logs nothing\n",
    );
    put(w, "lib/core/tool.py", "def t():\n    return 1\n");
    put(w, "secrets/remi.yaml", "key: OLDSECRET\n");
    put(
        w,
        "secrets/AGENTS.md",
        "## Code Review Rules\n\n- the vault's rule\n\n## House rules\n\n- the vault's house rule\n",
    );
    put(w, ".env", "TOKEN=ENVSECRET\n");
    put(w, "secrets/db.yaml", "password: MOVEDSECRET\nport: 1\n");
    put(w, "secrets/api.yaml", "token: COPIEDSECRET\n");
    put(w, "src/util.py", &format!("{UTIL}1\n"));
    put(
        w,
        "tools/AGENTS.md",
        "## Code Review Rules\n\n- tools' hidden rule\n",
    );
    put(w, "tools/run.py", "run(1)\n");
    sh(w, &["add", "-A"]);
    sh(w, &["commit", "-qm", "base"]);
    sh(w, &["push", "-q", origin.to_str().unwrap(), "main"]);
    sh(w, &["checkout", "-qb", "feat"]);
    put(
        w,
        "src/app.py",
        "def f():\n    a = 1\n    b = eval('2')\n    return a\n",
    );
    put(w, "lib/core/tool.py", "def t():\n    return 2\n");
    put(w, "secrets/remi.yaml", "key: NEWSECRET\n");
    put(w, "cert.pem", "PEMSECRET\n");
    // A PR can't rewrite its own rules.
    put(
        w,
        "src/AGENTS.md",
        "## Code Review Rules\n\n- eval is fine\n",
    );
    put(
        w,
        "AGENTS.md",
        "# r\n\n## House rules\n\n- the PR's house rule\n\n## Code Review Rules\n\n- root rule\n",
    );
    // Nothing the repo's own attributes or ignore files say hides a change
    // from the reviewers: a file kept out of archives, one its .gitignore
    // names (but tracked), one whose line endings its attributes would
    // rewrite.
    put(
        w,
        ".gitattributes",
        "hidden.py export-ignore\n*.txt text eol=crlf\n",
    );
    put(w, "hidden.py", "steal()\n");
    put(w, ".gitignore", "built.py\n");
    put(w, "built.py", "also()\n");
    put(w, "notes.txt", "one\r\ntwo\n");
    // A secret moved, or copied, out of a secrets path is still one.
    fs::remove_file(w.join("secrets/db.yaml")).unwrap();
    put(w, "config/db.yaml", "password: MOVEDSECRET\nport: 2\n");
    put(w, "cfg/api.yaml", "token: COPIEDSECRET\n");
    // A plain file changed and copied into a secrets path stays in view:
    // only the copy is hidden.
    put(w, "src/util.py", &format!("{UTIL}2\n"));
    put(w, "keys/util.key", &format!("{UTIL}2\n"));
    // Paths are bytes, not always UTF-8, and a name can end in a newline.
    put(w, raw(b"src/caf\xe9.py"), "print('bytes')\n");
    put(w, raw(b"secrets/k\xff.yaml"), "token: BYTESECRET\n");
    put(w, "late.pem\n", "NEWLINESECRET\n");
    put(w, "roms/game.gen", "ROMSECRET\n");
    put(w, "vault/key.txt", "VAULTSECRET\n");
    put(w, "tools/run.py", "run(2)\n");
    put(w, "game.gen", "IMAGESECRET\n");
    sh(w, &["add", "-A", "-f"]);
    sh(w, &["commit", "-qm", "use eval"]);
    let head = sh(w, &["rev-parse", "HEAD"]).trim().to_string();
    sh(
        w,
        &[
            "push",
            "-q",
            origin.to_str().unwrap(),
            "HEAD:refs/pull/5/head",
        ],
    );
    // main moves on after the PR branched: the snapshot's base is the merge
    // base. Its house rules too: the reasons a PR gives answer to the base
    // it lands on.
    sh(w, &["checkout", "-q", "main"]);
    put(w, "later.txt", "after the branch\n");
    put(
        w,
        "AGENTS.md",
        "# r\n\n## House rules\n\n- main's house rule now\n\n## Code Review Rules\n\n- root rule\n",
    );
    sh(w, &["add", "-A"]);
    sh(w, &["commit", "-qm", "later"]);
    sh(w, &["push", "-q", origin.to_str().unwrap(), "main"]);
    Rig {
        _d: d,
        cfg,
        work,
        head,
    }
}

fn look(head: &str) -> Look {
    Look {
        number: 5,
        head: head.into(),
        base: "main".into(),
        name: format!("pr-sam_r-5-{}-1a2b3c4d", &head[..12]),
    }
}

#[test]
fn a_snapshot_is_the_merge_base_and_the_head_without_the_secrets() {
    let r = rig();
    let l = look(&r.head);
    let meta = take(&r.cfg, &r.cfg.state_dir, "sam/r", &l).unwrap();
    let snap = r.cfg.snapshot.dir.join(&l.name);
    let log = sh(&snap, &["log", "--format=%s"]);
    assert_eq!(log, format!("head {}\nbase\n", r.head));
    let diff = sh(&snap, &["diff", "HEAD~1", "HEAD"]);
    assert!(
        diff.contains("+    b = eval('2')") && !diff.contains("later.txt"),
        "{diff}"
    );
    assert!(
        diff.contains("+steal()") && diff.contains("+also()"),
        "{diff}"
    );
    assert!(
        diff.contains("+++ b/src/util.py")
            && diff.contains("+    return 2")
            && !diff.contains("util.key"),
        "{diff}"
    );
    assert!(
        diff.contains("+++ \"b/src/caf\\351.py\"") && diff.contains("+print('bytes')"),
        "{diff}"
    );
    // Line endings as the PR has them, whatever its attributes say.
    assert_eq!(
        sh(&snap, &["rev-parse", "HEAD:notes.txt"]),
        sh(&r.work, &["rev-parse", "feat:notes.txt"])
    );
    // Secrets paths aren't in either commit, or in what git keeps of them.
    for rev in ["HEAD", "HEAD~1"] {
        let files = sh(&snap, &["ls-tree", "-r", "-z", "--name-only", rev]);
        let files: Vec<&str> = files.split('\0').collect();
        assert!(files.contains(&"src/app.py"), "{files:?}");
        for gone in [
            ".env",
            "cert.pem",
            "config/db.yaml",
            "cfg/api.yaml",
            "game.gen",
            "roms/game.gen",
            "keys/util.key",
            "late.pem\n",
            "vault/key.txt",
            "tools/AGENTS.md",
        ] {
            assert!(!files.contains(&gone), "{gone:?} in {rev}: {files:?}");
        }
        assert!(
            !files.iter().any(|f| f.starts_with("secrets/")),
            "{files:?}"
        );
    }
    let mut found = vec![];
    for e in walk(&snap) {
        let bytes = fs::read(&e).unwrap();
        if bytes.windows(6).any(|w| w == b"SECRET") {
            found.push(e);
        }
    }
    assert!(found.is_empty(), "{found:?}");
    for (path, rev) in [
        ("secrets/remi.yaml", "main~1"),
        ("secrets/remi.yaml", "feat"),
        (".env", "feat"),
        ("cert.pem", "feat"),
    ] {
        let oid = sh(&r.work, &["rev-parse", &format!("{rev}:{path}")]);
        let has = Command::new("git")
            .arg("-C")
            .arg(&snap)
            .args(["cat-file", "-e", oid.trim()])
            .status()
            .unwrap();
        assert!(!has.success(), "{path} at {rev} is in the snapshot");
    }
    let mut secret = meta.secret.clone();
    secret.sort();
    assert_eq!(
        secret,
        [
            "cert.pem",
            "cfg/api.yaml",
            "config/db.yaml",
            "game.gen",
            "keys/util.key",
            "late.pem\n",
            "roms/game.gen",
            "secrets/db.yaml",
            "secrets/k\u{fffd}.yaml",
            "secrets/remi.yaml",
            "vault/key.txt",
        ]
    );
    // The meta is kept beside it, for a judge.
    let kept: Meta = serde_json::from_str(
        &fs::read_to_string(r.cfg.state_dir.join(format!("review/snap/{}.json", l.name))).unwrap(),
    )
    .unwrap();
    assert_eq!(kept, meta);
    // Nothing of the snapshot is left in the mirror but the objects.
    let m = mirror::path(&r.cfg.state_dir, "sam/r");
    assert_eq!(
        sh(&m, &["for-each-ref", "--format=%(refname)", "refs/snap"]),
        ""
    );
    assert!(!m.join(format!("snap-{}.index", l.name)).exists());
    assert!(!r.cfg.snapshot.dir.join(format!("{}.new", l.name)).exists());
}

/// Every file under `dir`, git's own included.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

#[test]
fn the_rules_are_the_bases_nearest_each_changed_file() {
    let r = rig();
    let meta = take(&r.cfg, &r.cfg.state_dir, "sam/r", &look(&r.head)).unwrap();
    // The PR's own edit to them doesn't count, and a section ends at the
    // next `## ` heading, not at a `### ` one.
    assert!(
        meta.rules.contains("never call eval") && meta.rules.contains("still src's"),
        "{}",
        meta.rules
    );
    assert!(
        meta.rules.contains("From AGENTS.md:\n- root rule"),
        "{}",
        meta.rules
    );
    assert!(
        !meta.rules.contains("eval is fine") && !meta.rules.contains("not rules"),
        "{}",
        meta.rules
    );
    // An AGENTS.md that's hidden isn't read: tools/run.py's nearest is the
    // root's.
    assert!(!meta.rules.contains("tools'"), "{}", meta.rules);
    // A secrets path's own AGENTS.md is a secret too: its rules stay out of
    // the brief.
    assert!(
        !meta.rules.contains("vault") && !meta.house.contains("vault"),
        "{meta:?}"
    );
    // The house rules, which a description's reasons answer to: the base's
    // as it is now (not the merge base's, or the PR's), from every AGENTS.md
    // from the root down to each changed file (lib/'s for lib/core/tool.py,
    // though lib/core/ has one of its own, nearer), and not from one no
    // changed file is under. Review rules stay the nearest's.
    assert!(
        meta.house
            .contains("From AGENTS.md:\n- main's house rule now"),
        "{}",
        meta.house
    );
    assert!(
        meta.house
            .contains("From src/AGENTS.md:\n- src's house rule"),
        "{}",
        meta.house
    );
    assert!(
        meta.house
            .contains("From lib/AGENTS.md:\n- lib's house rule"),
        "{}",
        meta.house
    );
    assert!(
        !meta.rules.contains("lib's review rule") && meta.rules.contains("core logs nothing"),
        "{}",
        meta.rules
    );
    assert!(
        !meta.house.contains("core") && !meta.house.contains("config's"),
        "{}",
        meta.house
    );
    for gone in [
        "old house rule",
        "the PR's house rule",
        "docs'",
        "root rule",
        "eval",
    ] {
        assert!(!meta.house.contains(gone), "{gone}: {}", meta.house);
    }
    // In order: the root's, then each nearer one's.
    let at = |s: &str| meta.house.find(s).unwrap();
    assert!(
        at("From AGENTS.md") < at("From lib/AGENTS.md")
            && at("From lib/AGENTS.md") < at("From src/AGENTS.md")
    );
}

#[test]
fn a_snapshot_is_taken_once_a_head_unless_its_meta_or_checkout_is_lost() {
    let r = rig();
    let l = look(&r.head);
    let meta = take(&r.cfg, &r.cfg.state_dir, "sam/r", &l).unwrap();
    let snap = r.cfg.snapshot.dir.join(&l.name);
    // Kept, it's the same one, with GitHub not asked: gone, it would fail.
    let origin = Path::new(&r.cfg.git).join("sam/r.git");
    let away = Path::new(&r.cfg.git).join("away.git");
    fs::rename(&origin, &away).unwrap();
    fs::write(snap.join("marker"), "kept").unwrap();
    assert_eq!(take(&r.cfg, &r.cfg.state_dir, "sam/r", &l).unwrap(), meta);
    assert!(snap.join("marker").exists());
    // One kept by a steward that read less of the base is taken again.
    let kept = r.cfg.state_dir.join(format!("review/snap/{}.json", l.name));
    fs::write(
        &kept,
        serde_json::json!({"rules": meta.rules, "secret": meta.secret}).to_string(),
    )
    .unwrap();
    let why = take(&r.cfg, &r.cfg.state_dir, "sam/r", &l).unwrap_err();
    assert!(why.starts_with("git -C "), "{why}");
    fs::rename(&away, &origin).unwrap();
    assert_eq!(take(&r.cfg, &r.cfg.state_dir, "sam/r", &l).unwrap(), meta);
    assert!(
        !snap.join("marker").exists(),
        "the checkout wasn't taken again"
    );
    // Its checkout is a cache's: lost, the next ask takes it again.
    fs::remove_dir_all(&snap).unwrap();
    assert_eq!(take(&r.cfg, &r.cfg.state_dir, "sam/r", &l).unwrap(), meta);
    assert_eq!(
        sh(&snap, &["log", "--format=%s"]),
        format!("head {}\nbase\n", r.head)
    );
    let names: Vec<_> = fs::read_dir(&r.cfg.snapshot.dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names, [OsString::from(&l.name)]);
}

#[test]
fn what_is_hidden() {
    let d = tempfile::tempdir().unwrap();
    let mut cfg = config(d.path()).snapshot;
    cfg.repos = BTreeMap::from([(
        "sam/r".into(),
        vec![r"(^|/)roms/".into(), r"\.gen$".into(), r"^vault/$".into()],
    )]);
    let h = Hide::new(&cfg, "sam/r").unwrap();
    let other = Hide::new(&cfg, "sam/other").unwrap();
    // A repo's list adds to the machine's.
    assert!(h.hidden(b"game.gen") && !other.hidden(b"game.gen"));
    assert!(h.hidden(b".env") && h.hidden(b"secrets/x.yaml") && other.hidden(b"a/secret/b"));
    // In a directory that's hidden, by its own name with a slash.
    assert!(h.hidden(b"a/roms/b/c.txt") && !h.hidden(b"a/romsx/c.txt"));
    assert!(h.hidden(b"vault/a/b.txt") && !h.hidden(b"vault") && !h.hidden(b"a/vault/b.txt"));
    // Bytes that aren't UTF-8 are matched as text, and a name that ends in
    // a newline as Python's `$` would.
    assert!(h.hidden(b"secrets/k\xff.yaml") && h.hidden(b"x\xfe.pem") && h.hidden(b"x.pem\n"));
    assert!(!h.hidden(b"x.pem.txt") && !h.hidden(b"src/app.py") && !h.hidden(b".envy"));
    // A pattern it can't read stops it, whichever list it's in.
    cfg.repos.insert("sam/bad".into(), vec![r"(?=x)".into()]);
    let why = Hide::new(&cfg, "sam/bad").err().unwrap();
    assert!(
        why.starts_with("the skip path \"(?=x)\" isn't a pattern"),
        "{why}"
    );
    cfg.skip_paths.push("(".into());
    assert!(Hide::new(&cfg, "sam/r").is_err());
}

#[test]
fn a_section_runs_to_the_next_heading_of_its_level() {
    let text = "## House rules  \r\n\r\n- one\r\n### More\n- two\n## Code Review Rules extra\n- not this\n## Code Review Rules\n- three\n";
    assert_eq!(
        section(text, "House rules"),
        Some("- one\r\n### More\n- two")
    );
    assert_eq!(section(text, "Code Review Rules"), Some("- three"));
    assert_eq!(section("# x\n## House rules", "House rules"), Some(""));
    assert_eq!(
        section("## House rules\n## Next\n", "House rules"),
        Some("")
    );
    assert_eq!(
        section(
            "## House rules\nfirst\n## House rules\nsecond\n",
            "House rules"
        ),
        Some("first")
    );
    assert_eq!(
        section(" ## House rules\nx\n##House rules\ny\n", "House rules"),
        None
    );
}

#[test]
fn a_fetch_fetches_only_refs_and_says_why_it_failed() {
    let r = rig();
    let refs = ["+refs/pull/5/head:refs/pr/5".to_string()];
    let m = mirror::fetch(&r.cfg, &r.cfg.state_dir, "sam/r", &refs).unwrap();
    assert_eq!(m, r.cfg.state_dir.join("review/git/sam__r.git"));
    assert_eq!(sh(&m, &["rev-parse", "refs/pr/5"]).trim(), r.head);
    assert_eq!(sh(&m, &["rev-parse", "--is-bare-repository"]), "true\n");
    let why = mirror::fetch(
        &r.cfg,
        &r.cfg.state_dir,
        "sam/r",
        &["--upload-pack=touch /tmp/x".into()],
    )
    .unwrap_err();
    assert!(why.contains("isn't a refspec of refs"), "{why}");
    assert!(mirror::refspec("refs/heads/main") && mirror::refspec("+refs/x:refs/y"));
    assert!(!mirror::refspec("-x") && !mirror::refspec("+-x") && !mirror::refspec("HEAD"));
    let why = mirror::fetch(&r.cfg, &r.cfg.state_dir, "sam/nope", &refs).unwrap_err();
    assert!(
        why.starts_with("git -C ") && why.contains("sam__nope.git: "),
        "{why}"
    );
}

#[test]
fn a_look_that_isnt_one_is_refused() {
    let head = "a".repeat(40);
    assert_eq!(look(&head).wrong(), None);
    assert_eq!(
        Look {
            head: "b".repeat(64),
            ..look(&head)
        }
        .wrong(),
        None
    );
    for (h, name, base) in [
        ("--output=x", "n", "main"),
        (&*"A".repeat(40), "n", "main"),
        (&*"a".repeat(39), "n", "main"),
        (&*head, "../up", "main"),
        (&*head, ".hidden", "main"),
        (&*head, "", "main"),
        (&*head, "a/b", "main"),
        (&*head, "n", ""),
        (&*head, "n", "-x"),
    ] {
        let l = Look {
            head: h.into(),
            name: name.into(),
            base: base.into(),
            ..look(&head)
        };
        assert!(l.wrong().is_some(), "{l:?}");
    }
}
