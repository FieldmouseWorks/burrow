//! `runners`, `trust` and `issue`: what each says of a machine laid out
//! under a temporary root, with systemctl and GitHub stand-ins.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::{Machine, Ran, Run};
use crate::query::Query;
use crate::{App, Config, Inbox, Journal, Runner, RunnerKind, Tools, Trust};

pub(super) const HASH: &str = "0123456789abcdfghijklmnpqrsvwxyz";

/// systemctl's answers by verb, and GitHub's one answer.
#[derive(Default)]
pub(super) struct Fake {
    pub(super) show: String,
    pub(super) list: String,
    pub(super) github: RefCell<Option<Result<Value, String>>>,
    pub(super) asked: RefCell<Vec<Vec<String>>>,
    pub(super) queries: RefCell<Vec<(String, String, Value)>>,
}

impl Run for Fake {
    fn run(&self, _program: &Path, args: &[&str], _user: bool, _uid: u32) -> Result<Ran, String> {
        self.asked
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        let stdout = match args.first() {
            Some(&"show") => self.show.clone(),
            Some(&"list-units") => self.list.clone(),
            _ => return Err("not scripted".into()),
        };
        Ok(Ran {
            code: Some(0),
            stdout,
            stderr: String::new(),
        })
    }

    fn github(
        &self,
        _gh: Option<&Path>,
        owner: &str,
        query: &str,
        variables: Value,
    ) -> Result<Value, String> {
        self.queries
            .borrow_mut()
            .push((owner.into(), query.into(), variables));
        self.github
            .borrow()
            .clone()
            .unwrap_or_else(|| Err("not scripted".into()))
    }
}

pub(super) fn config(root: &Path) -> Config {
    Config {
        jobs: None,
        projects: None,
        machinery: Some(burrow_github::Author {
            login: "workers[bot]".into(),
            id: 300,
        }),
        repo: "sam/box".into(),
        label: "probe".into(),
        authors: vec![burrow_github::Author {
            login: "sam".into(),
            id: 1,
        }],
        host: "box".into(),
        user: "dev".into(),
        journal: Journal {
            units: vec![],
            user_units: vec![],
        },
        state_dir: root.to_path_buf(),
        system_repo: None,
        proj: None,
        tools: Tools {
            git: "git".into(),
            journalctl: "/bin/journalctl".into(),
            systemctl: "/bin/systemctl".into(),
            gh: Some("/run/current-system/sw/bin/gh".into()),
        },
        runners: vec![named(), fresh()],
        trust: None,
        hidden: vec!["(^|/)secrets?/".into(), "\\.(pem|key|p12|pfx)$".into()],
    }
}

pub(super) fn named() -> Runner {
    Runner {
        name: "ci-runner".into(),
        kind: RunnerKind::Named,
        repos: vec!["sam/box".into()],
        labels: vec![
            "self-hosted".into(),
            "Linux".into(),
            "X64".into(),
            "box".into(),
        ],
        defined: true,
        user: Some("ci".into()),
        dynamic_user: false,
        home: Some("/var/lib/ci".into()),
        packages: vec![],
        environment: BTreeMap::from([(
            "PATH".to_string(),
            "/etc/actions-runner/ci-runner/bin:/run/current-system/sw/bin".to_string(),
        )]),
    }
}

pub(super) fn fresh() -> Runner {
    Runner {
        name: "burrow-fresh-runner@".into(),
        kind: RunnerKind::Fresh,
        repos: vec!["sam/box".into()],
        labels: vec!["fresh-box".into()],
        defined: true,
        user: None,
        dynamic_user: true,
        home: None,
        packages: vec!["git-2.51.0".into(), "jq-1.8.1".into()],
        environment: BTreeMap::from([
            (
                "PATH".to_string(),
                format!("/nix/store/{HASH}-git-2.51.0/bin:/run/current-system/sw/bin"),
            ),
            (
                "HOME".to_string(),
                "/var/lib/private/burrow-fresh-runner/%i".to_string(),
            ),
        ]),
    }
}

pub(super) fn machine<'a>(cfg: &'a Config, root: &Path, run: &'a Fake) -> Machine<'a> {
    Machine {
        cfg,
        root: root.to_path_buf(),
        now: 1_793_500_000,
        uid: 1001,
        run,
    }
}

/// A store package's command, and a link to it from `dir`.
pub(super) fn command(root: &Path, dir: &str, package: &str, name: &str) {
    let store = root.join(format!("nix/store/{HASH}-{package}/bin"));
    fs::create_dir_all(&store).unwrap();
    let file = store.join(name);
    burrow_test::executable(&file, "#!/bin/sh\n");
    let dir = root.join(dir.trim_start_matches('/'));
    fs::create_dir_all(&dir).unwrap();
    symlink(
        format!("/nix/store/{HASH}-{package}/bin/{name}"),
        dir.join(name),
    )
    .unwrap();
}

/// The machine both runners share: a system profile with git and coreutils,
/// the named runner's own bin with git, and no Rust anywhere.
pub(super) fn base(root: &Path) {
    command(root, "/run/current-system/sw/bin", "coreutils-9.8", "ls");
    command(root, "/run/current-system/sw/bin", "git-2.51.0", "git");
    command(
        root,
        "/etc/actions-runner/ci-runner/bin",
        "git-2.51.0",
        "git",
    );
    command(
        root,
        "/etc/actions-runner/ci-runner/bin",
        "nodejs-22.20.0",
        "node",
    );
    // The fresh runner's own store directory, files in it.
    let git = root.join(format!("nix/store/{HASH}-git-2.51.0/bin"));
    fs::create_dir_all(git).unwrap();
}

pub(super) fn show(path: &str) -> String {
    format!(
        "Id=ci-runner.service\nLoadState=loaded\nActiveState=active\nSubState=running\nActiveEnterTimestamp=Fri 2026-11-13 01:00:00 UTC\nUser=ci\nDynamicUser=no\nNeedDaemonReload=no\nEnvironment=PATH={path} LOCALE_ARCHIVE=/x \"TOKEN=ghp_notreported\"\n"
    )
}

#[test]
fn named_and_fresh_runners_without_rust_say_there_is_no_default_toolchain() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    base(root);
    let cfg = config(root);
    let fake = Fake {
        show: show("/etc/actions-runner/ci-runner/bin:/run/current-system/sw/bin"),
        ..Fake::default()
    };
    let got = machine(&cfg, root, &fake)
        .answer(&Query::Runners { name: None })
        .unwrap();
    assert!(
        got.contains(
            "ci-runner: a named runner for sam/box (labels: self-hosted, Linux, X64, box)"
        ),
        "{got}"
    );
    assert!(
        got.contains("configured: user ci, HOME /var/lib/ci"),
        "{got}"
    );
    assert!(
        got.contains(
            "loaded: ci-runner.service active/running since Fri 2026-11-13 01:00:00 UTC, user ci"
        ),
        "{got}"
    );
    assert!(
        got.contains("found: 2 of 2 PATH directories, 4 commands"),
        "{got}"
    );
    assert!(
        got.contains("found rust: rustc not on PATH, cargo not on PATH, rustup not on PATH; default toolchain: none: neither rustc nor rustup is on PATH"),
        "{got}"
    );
    // The fresh runners: configured from the template, no instance loaded.
    assert!(got.contains("burrow-fresh-runner@INSTANCE: runners started fresh for one job each, for sam/box (label: fresh-box)"), "{got}");
    assert!(got.contains("configured: user a dynamic user of its own, HOME /var/lib/private/burrow-fresh-runner/%i"), "{got}");
    assert!(
        got.contains("configured packages: git-2.51.0, jq-1.8.1"),
        "{got}"
    );
    assert!(
        got.contains("loaded: no instance is loaded now, so only its configuration says"),
        "{got}"
    );
    assert_eq!(
        got.matches("default toolchain: none: neither rustc nor rustup is on PATH")
            .count(),
        2,
        "{got}"
    );
    // Only the environment keys it tells.
    assert!(
        !got.contains("ghp_notreported") && !got.contains("LOCALE_ARCHIVE"),
        "{got}"
    );
    // Nothing was run but systemctl's reads.
    for asked in fake.asked.borrow().iter() {
        assert!(
            ["show", "list-units"].contains(&asked[0].as_str()),
            "{asked:?}"
        );
    }
}

#[test]
fn rustup_on_path_reads_the_default_its_home_holds() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    base(root);
    for name in ["rustup", "rustc", "cargo"] {
        command(
            root,
            "/etc/actions-runner/ci-runner/bin",
            "rustup-1.28.2",
            name,
        );
    }
    let cfg = config(root);
    let fake = Fake {
        show: show("/etc/actions-runner/ci-runner/bin:/run/current-system/sw/bin"),
        ..Fake::default()
    };
    let m = machine(&cfg, root, &fake);
    let ask = || {
        m.answer(&Query::Runners {
            name: Some("ci-runner".into()),
        })
        .unwrap()
    };

    // No settings at all: rustup was never set up for the user.
    let got = ask();
    assert!(
        got.contains("rustc in /etc/actions-runner/ci-runner/bin (rustup-1.28.2)"),
        "{got}"
    );
    assert!(
        got.contains("default toolchain: none: /var/lib/ci/.rustup/settings.toml isn't there"),
        "{got}"
    );
    // In full: each directory's packages, and the other tools.
    assert!(got.contains("/etc/actions-runner/ci-runner/bin: 5 commands from git-2.51.0, nodejs-22.20.0, rustup-1.28.2"), "{got}");
    assert!(
        got.contains("found tools: git in /etc/actions-runner/ci-runner/bin (git-2.51.0)"),
        "{got}"
    );
    assert!(got.contains("nix not on PATH"), "{got}");

    // Settings with no default.
    let settings = root.join("var/lib/ci/.rustup/settings.toml");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    fs::write(&settings, "version = \"12\"\nprofile = \"minimal\"\n").unwrap();
    assert!(ask().contains(
        "default toolchain: none: /var/lib/ci/.rustup/settings.toml names no default_toolchain"
    ));

    // And with one.
    fs::write(
        &settings,
        "default_toolchain = \"stable-x86_64-unknown-linux-gnu\"\nversion = \"12\"\n",
    )
    .unwrap();
    assert!(ask().contains(
        "default toolchain: stable-x86_64-unknown-linux-gnu (/var/lib/ci/.rustup/settings.toml)"
    ));

    // The environment's toolchain wins.
    let mut cfg = config(root);
    cfg.runners[0]
        .environment
        .insert("RUSTUP_TOOLCHAIN".into(), "1.91.0".into());
    let fake = Fake {
        show: show("/etc/actions-runner/ci-runner/bin:/run/current-system/sw/bin"),
        ..Fake::default()
    };
    let got = machine(&cfg, root, &fake)
        .answer(&Query::Runners {
            name: Some("ci-runner".into()),
        })
        .unwrap();
    assert!(got.contains("configured RUSTUP_TOOLCHAIN=1.91.0"), "{got}");
    assert!(
        got.contains("loaded: no RUSTUP_TOOLCHAIN, though one is configured"),
        "{got}"
    );
    // The loaded unit lacks it: the newer configuration doesn't restore it.
    assert!(
        got.contains(
            "default toolchain: stable-x86_64-unknown-linux-gnu (/var/lib/ci/.rustup/settings.toml)"
        ),
        "{got}"
    );
    assert!(!got.contains("1.91.0 (RUSTUP_TOOLCHAIN"), "{got}");
    // Not loaded at all: the configuration is all there is.
    let quiet = Fake::default();
    let got = machine(&cfg, root, &quiet)
        .answer(&Query::Runners {
            name: Some("ci-runner".into()),
        })
        .unwrap();
    assert!(
        got.contains("default toolchain: 1.91.0 (RUSTUP_TOOLCHAIN in its environment)"),
        "{got}"
    );
}

#[test]
fn a_fresh_runner_with_rustup_has_no_default_until_its_job_sets_one() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    base(root);
    command(
        root,
        "/run/current-system/sw/bin",
        "rustup-1.28.2",
        "rustup",
    );
    let cfg = config(root);
    let fake = Fake {
        list: "burrow-fresh-runner@1-2.service loaded active running GitHub Actions runner 1-2\n".into(),
        show: "Id=burrow-fresh-runner@1-2.service\nLoadState=loaded\nActiveState=active\nSubState=running\nDynamicUser=yes\nNeedDaemonReload=yes\nEnvironment=PATH=/run/current-system/sw/bin HOME=/var/lib/private/burrow-fresh-runner/1-2\n".into(),
        ..Fake::default()
    };
    let got = machine(&cfg, root, &fake)
        .answer(&Query::Runners {
            name: Some("burrow-fresh-runner@".into()),
        })
        .unwrap();
    assert!(got.contains("loaded: burrow-fresh-runner@1-2.service active/running, a dynamic user; its unit file changed since systemd loaded it"), "{got}");
    assert!(
        got.contains("loaded PATH=/run/current-system/sw/bin"),
        "{got}"
    );
    // Its HOME is the instance's own, which the probe can't read: unknown,
    // never a guess.
    assert!(
        got.contains("rustup in /run/current-system/sw/bin (rustup-1.28.2)"),
        "{got}"
    );
    assert!(
        got.contains("default toolchain: none: /var/lib/private/burrow-fresh-runner/1-2/.rustup/settings.toml isn't there"),
        "{got}"
    );
    // With no instance, the template's HOME says it starts empty.
    let fake = Fake::default();
    let got = machine(&cfg, root, &fake)
        .answer(&Query::Runners {
            name: Some("burrow-fresh-runner@".into()),
        })
        .unwrap();
    assert!(got.contains("default toolchain: none until a job sets one: /var/lib/private/burrow-fresh-runner/%i/.rustup/settings.toml is under each instance's own state"), "{got}");
}

#[test]
fn an_unreadable_home_is_unknown_and_an_unknown_runner_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    base(root);
    command(
        root,
        "/etc/actions-runner/ci-runner/bin",
        "rustup-1.28.2",
        "rustup",
    );
    let home = root.join("var/lib/ci");
    fs::create_dir_all(home.join(".rustup")).unwrap();
    fs::write(
        home.join(".rustup/settings.toml"),
        "default_toolchain = \"x\"\n",
    )
    .unwrap();
    burrow_test::permissions(&home, 0o000);
    let cfg = config(root);
    let fake = Fake::default();
    let m = machine(&cfg, root, &fake);
    let got = m.answer(&Query::Runners {
        name: Some("ci-runner".into()),
    });
    burrow_test::permissions(&home, 0o755);
    let got = got.unwrap();
    // Root reads anything: the test only means something as a user.
    if !fs::read_to_string("/proc/self/status")
        .unwrap()
        .contains("Uid:\t0\t")
    {
        assert!(
            got.contains("default toolchain: /var/lib/ci/.rustup/settings.toml: unknown: the probe may not read it"),
            "{got}"
        );
    }
    assert!(
        got.contains("loaded: systemd says nothing of ci-runner.service"),
        "{got}"
    );
    let refused = m
        .answer(&Query::Runners {
            name: Some("other".into()),
        })
        .unwrap_err();
    assert_eq!(
        refused,
        "\"other\" isn't one of this machine's runners: ci-runner, burrow-fresh-runner@"
    );
}

#[test]
fn hidden_paths_are_never_listed_resolved_or_read() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    base(root);
    command(
        root,
        "/etc/actions-runner/ci-runner/bin",
        "rustup-1.28.2",
        "rustup",
    );
    // A secret, a PATH directory under it, a command linked into it, and
    // rustup's settings linked to it.
    let secret = root.join("run/secrets/token");
    fs::create_dir_all(secret.parent().unwrap()).unwrap();
    fs::write(&secret, "default_toolchain = \"SECRET-TOOLCHAIN\"\n").unwrap();
    command(root, "/run/secrets/bin", "leak-1", "leak");
    symlink(
        "/run/secrets/token",
        root.join("etc/actions-runner/ci-runner/bin/rustc"),
    )
    .unwrap();
    let settings = root.join("var/lib/ci/.rustup");
    fs::create_dir_all(&settings).unwrap();
    symlink("/run/secrets/token", settings.join("settings.toml")).unwrap();
    // A link to a directory that leads into the hidden one.
    symlink("/run/secrets/bin", root.join("etc/sneaky")).unwrap();
    let mut cfg = config(root);
    cfg.runners[0].environment.insert(
        "PATH".into(),
        "/etc/actions-runner/ci-runner/bin:/run/secrets/bin:/etc/sneaky".into(),
    );
    let fake = Fake::default();
    let got = machine(&cfg, root, &fake)
        .answer(&Query::Runners {
            name: Some("ci-runner".into()),
        })
        .unwrap();
    assert!(
        !got.contains("SECRET-TOOLCHAIN") && !got.contains("leak"),
        "{got}"
    );
    assert!(
        got.contains(
            "/run/secrets/bin (not read: it is, or leads to, a path the reviewers never read)"
        ),
        "{got}"
    );
    assert!(
        got.contains("/etc/sneaky (not read: it is, or leads to, a path the reviewers never read)"),
        "{got}"
    );
    assert!(got.contains("1 left out as hidden"), "{got}");
    assert!(got.contains("rustc unknown: not found in the directories searched; the probe could not search /etc/actions-runner/ci-runner/bin (not read: it is, or leads to, a path the reviewers never read)"), "{got}");
    assert!(
        got.contains("rustup's own answer: /var/lib/ci/.rustup/settings.toml: not read: it is, or leads to, a path the reviewers never read"),
        "{got}"
    );

    // Patterns that don't compile read nothing at all.
    cfg.hidden.push("(".into());
    let got = machine(&cfg, root, &fake)
        .answer(&Query::Runners {
            name: Some("ci-runner".into()),
        })
        .unwrap();
    assert!(got.contains("found: 0 of 3 PATH directories"), "{got}");
    assert!(
        got.contains("the reviewers' hidden paths don't all compile, so no file is read"),
        "{got}"
    );
}

fn trust(root: &Path) -> Trust {
    Trust {
        inbox: Some(Inbox {
            authors: vec![
                burrow_github::Author {
                    login: "sam".into(),
                    id: 1,
                },
                burrow_github::Author {
                    login: "steward[bot]".into(),
                    id: 200,
                },
            ],
            vm: false,
        }),
        engines: None,
        apps: vec![
            App {
                name: "workers".into(),
                login: "workers[bot]".into(),
                id: 300,
            },
            App {
                name: "steward".into(),
                login: "steward[bot]".into(),
                id: 200,
            },
        ],
        machinery: Some("workers".into()),
        autoland: Some(PathBuf::from("/etc/burrow/autoland.json")),
        sandboxes: true,
    }
    .tap(root)
}

trait Tap {
    fn tap(self, root: &Path) -> Self;
}
impl Tap for Trust {
    /// The steward's deployed config, under `root`.
    fn tap(self, root: &Path) -> Self {
        let file = root.join("etc/burrow/autoland.json");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(
            &file,
            json!({"authors": ["sam"], "bots": ["dependabot", "renovate", "workers"], "app": {"slug": "steward", "id": 9}, "majorKey": "/var/lib/x/verdict.key"}).to_string(),
        )
        .unwrap();
        self
    }
}

#[test]
fn trust_says_the_deployed_authors_and_what_each_job_acts_as() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut cfg = config(root);
    cfg.trust = Some(trust(root));
    let fake = Fake::default();
    let got = machine(&cfg, root, &fake).answer(&Query::Trust).unwrap();
    for line in [
        "- inbox (an issue or reply becomes a job): sam (1), steward[bot] (200); issues the machinery App workers[bot] (300) opens run too, though its comments aren't heard",
        "- probe (issues in sam/box): sam (1), and the machinery App workers[bot] (300)",
        "- engines door: off",
        "- steward (autoland): PRs by sam and the bots dependabot, renovate, workers merge themselves once settled (by login); it acts as steward[bot], and its reviewers hold no GitHub token",
        "- a Claude job with no --app: dev's own gh login",
        "- a Codex job with no --app: the App workers[bot] (300)",
        "- a job with --app NAME: that App; this machine's are workers (workers[bot] (300)), steward (steward[bot] (200))",
        "- an inbox job on a bot's issue: the App workers[bot] (300) (Codex too)",
        "- a job in an agent sandbox (proj run --vm): no GitHub token",
        "- the machine's own calls (this probe, proj-inbox's posts, the dispatcher, Renovate): the App workers[bot] (300)",
    ] {
        assert!(got.contains(line), "{line}\n{got}");
    }
    assert!(!got.contains("verdict.key"), "{got}");

    // No machinery: a Codex job doesn't start, the machine calls as the user.
    let t = cfg.trust.as_mut().unwrap();
    t.machinery = None;
    t.inbox.as_mut().unwrap().vm = true;
    let got = machine(&cfg, root, &fake).answer(&Query::Trust).unwrap();
    assert!(
        got.contains(
            "- a Codex job with no --app: nothing: with no machinery App a Codex job doesn't start"
        ),
        "{got}"
    );
    assert!(
        got.contains("- an inbox job: in an agent sandbox (inbox.vm), with no GitHub login"),
        "{got}"
    );
    assert!(got.contains("- the machine's own calls (this probe, proj-inbox's posts, the dispatcher, Renovate): dev's own gh login"), "{got}");

    // A steward config that leads somewhere hidden isn't read.
    fs::remove_file(root.join("etc/burrow/autoland.json")).unwrap();
    fs::create_dir_all(root.join("run/secrets")).unwrap();
    fs::write(
        root.join("run/secrets/autoland.json"),
        "{\"authors\":[\"SECRET\"]}",
    )
    .unwrap();
    symlink(
        "/run/secrets/autoland.json",
        root.join("etc/burrow/autoland.json"),
    )
    .unwrap();
    let got = machine(&cfg, root, &fake).answer(&Query::Trust).unwrap();
    assert!(!got.contains("SECRET"), "{got}");
    assert!(
        got.contains("- steward (autoland): /etc/burrow/autoland.json: not read"),
        "{got}"
    );

    // A configuration from before `trust` says so.
    cfg.trust = None;
    assert!(machine(&cfg, root, &fake).answer(&Query::Trust).is_err());
}

fn who(login: &str, id: u64) -> Value {
    json!({"login": login, "__typename": if login.ends_with("[bot]") { "Bot" } else { "User" }, "databaseId": id})
}

fn edits(n: usize, total: usize) -> Value {
    let nodes: Vec<Value> = (0..n)
        .map(|i| {
            json!({"editedAt": "2026-11-01T00:00:00Z", "deletedAt": null, "editor": who("sam", 1), "deletedBy": null,
                   "diff": format!("SECRET EDIT {i}")})
        })
        .collect();
    json!({"totalCount": total, "nodes": nodes})
}

fn issue_data(edits: Value) -> Value {
    json!({"repository": {"issue": {
        "state": "OPEN", "createdAt": "2026-11-01T00:00:00Z",
        "title": "SECRET TITLE", "body": "SECRET BODY",
        "author": who("sam", 1),
        "labels": {"totalCount": 2, "nodes": [{"name": "claude"}, {"name": "ready"}]},
        "userContentEdits": edits,
        "timelineItems": {"totalCount": 1, "nodes": [{"createdAt": "2026-11-02T00:00:00Z", "actor": who("mallory", 99)}]}
    }}})
}

fn ask_issue(
    cfg: &Config,
    root: &Path,
    data: Result<Value, String>,
) -> (Result<String, String>, Fake) {
    let fake = Fake::default();
    *fake.github.borrow_mut() = Some(data);
    let got = machine(cfg, root, &fake).answer(&Query::Issue {
        repo: "sam/box".into(),
        number: 210,
    });
    (got, fake)
}

#[test]
fn an_issue_says_its_writers_and_how_much_history_github_kept() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut cfg = config(root);
    cfg.trust = Some(trust(root));

    // Below the cap: complete.
    let (got, fake) = ask_issue(&cfg, root, Ok(issue_data(edits(3, 3))));
    let got = got.unwrap();
    assert!(
        got.starts_with("sam/box#210: open, opened 2026-11-01T00:00:00Z\n"),
        "{got}"
    );
    assert!(
        got.contains("Body edits: 3 edits, all retained and returned"),
        "{got}"
    );
    assert!(
        got.contains("Title renames: 1 renames, all retained and returned"),
        "{got}"
    );
    assert!(
        got.contains("- sam (User 1): opened it, 3 edits; an inbox author, a probe author"),
        "{got}"
    );
    assert!(
        got.contains("- mallory (User 99): 1 rename; on none of the deployed trust lists"),
        "{got}"
    );
    assert!(got.contains("Labels (2): claude, ready"), "{got}");
    assert!(!got.contains("SECRET"), "{got}");
    let queries = fake.queries.borrow();
    let (owner, query, variables) = &queries[0];
    assert_eq!(owner, "sam");
    assert_eq!(variables, &json!({"owner": "sam", "name": "box", "n": 210}));
    // It asks for no text at all.
    let text = regex::Regex::new(r"\b(body|bodyText|bodyHTML|title|diff|lastEditedAt)\b").unwrap();
    assert!(!text.is_match(query), "{query}");

    // At the cap: the writers are the retained ones only.
    let (got, _) = ask_issue(&cfg, root, Ok(issue_data(edits(100, 100))));
    let got = got.unwrap();
    assert!(got.contains("Body edits: 100 edits retained, at GitHub's cap of 100: older ones may be gone with their writers"), "{got}");

    // Beyond what was returned: incomplete.
    let (got, _) = ask_issue(&cfg, root, Ok(issue_data(edits(100, 150))));
    let got = got.unwrap();
    assert!(got.contains("Body edits: 100 of the 150 edits GitHub reports were returned: the history is incomplete"), "{got}");

    // No count at all: said to be possibly incomplete.
    let mut data = issue_data(edits(1, 1));
    data["repository"]["issue"]["userContentEdits"]["totalCount"] = Value::Null;
    let got = ask_issue(&cfg, root, Ok(data)).0.unwrap();
    assert!(
        got.contains("1 edits returned; GitHub didn't say how many it keeps"),
        "{got}"
    );

    // A deleted account's edit, and one edit deleted by the App.
    let data = issue_data(json!({"totalCount": 2, "nodes": [
        {"editedAt": "x", "deletedAt": null, "editor": null, "deletedBy": null},
        {"editedAt": "y", "deletedAt": "z", "editor": who("sam", 1), "deletedBy": who("workers[bot]", 300)}
    ]}));
    let got = ask_issue(&cfg, root, Ok(data)).0.unwrap();
    assert!(got.contains("- unknown: an account GitHub no longer has (deleted, or hidden from the probe's login): 1 edit"), "{got}");
    assert!(
        got.contains("- workers[bot] (Bot 300): 1 edit deleted; the App workers"),
        "{got}"
    );

    // Not there, and GitHub failing.
    let missing = ask_issue(&cfg, root, Ok(json!({"repository": {"issue": null}})))
        .0
        .unwrap_err();
    assert!(
        missing.contains("isn't an issue the probe's GitHub login can see"),
        "{missing}"
    );
    let missing = ask_issue(&cfg, root, Ok(json!({"repository": null})))
        .0
        .unwrap_err();
    assert!(missing.contains("sam/box isn't there"), "{missing}");
    let failed = ask_issue(
        &cfg,
        root,
        Err(format!("POST graphql: {}", "x".repeat(2000))),
    )
    .0
    .unwrap_err();
    assert!(
        failed.starts_with("GitHub didn't say: POST graphql: ") && failed.len() < 600,
        "{failed}"
    );
}
