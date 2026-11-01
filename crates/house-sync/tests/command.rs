//! house-sync as its units run it: the config file, gh on PATH (here a
//! stand-in that keeps what it was asked, and as whom) and the notifier.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

fn house() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../house")
}

fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// A gh that answers `gh api -i` for one repo, sam/a, with no AGENTS.md,
/// and logs each call with the account it was told it's about.
const GH: &str = r#"#!/bin/sh
echo "${GH_TOKEN_OWNER:--} $*" >>"$LOG"
cat >/dev/null
printf 'HTTP/2.0 '
case "$*" in
  *" graphql --input -")
    printf '200 OK\n\n{"data":{"repositoryOwner":{"repositories":{"nodes":[{"nameWithOwner":"sam/a","isArchived":false,"defaultBranchRef":{"name":"main"}}],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}' ;;
  *" repos/sam/a")
    printf '200 OK\n\n{"full_name":"sam/a","default_branch":"main","owner":{"login":"sam"},"archived":false,"fork":false}' ;;
  *" repos/sam/a/git/ref/heads/main")
    printf '200 OK\n\n{"object":{"sha":"c0ffee"}}' ;;
  *)
    printf '404 Not Found\n\n{"message":"Not Found"}' ;;
esac
"#;

/// gh and the notifier, written once for every test: each keeps what it
/// was asked in the file its variable names. A file being written while
/// another test starts a process is open for writing in that process until
/// it execs, and running the file then fails with "Text file busy". No test
/// starts one before these are written, and none is written after.
fn stand_ins() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        script(&bin.path().join("gh"), GH);
        script(
            &bin.path().join("notify"),
            "#!/bin/sh\necho \"$1 $2\" >>\"$NOTICES\"\n",
        );
        bin
    })
    .path()
}

struct Machine {
    dir: tempfile::TempDir,
}

impl Machine {
    fn new(dry_run: bool) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "house": house(),
            "current": house(),
            "lock": dir.path().join("lock"),
            "owners": ["Sam"],
            "exclude": [],
            "notify": stand_ins().join("notify"),
            "dryRun": dry_run,
        });
        fs::write(dir.path().join("config.json"), config.to_string()).unwrap();
        Machine { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        let d = self.dir.path();
        let path = format!(
            "{}:{}",
            stand_ins().display(),
            std::env::var("PATH").unwrap()
        );
        Command::new(env!("CARGO_BIN_EXE_house-sync"))
            .arg(d.join("config.json"))
            .args(args)
            .env("PATH", path)
            .env_remove("GH_TOKEN_OWNER")
            .env("LOG", d.join("gh.log"))
            .env("NOTICES", d.join("notices"))
            .output()
            .unwrap()
    }

    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.path().join(name)).unwrap_or_default()
    }
}

#[test]
fn a_pass_asks_gh_as_the_owner_it_lists_and_tells_the_notifier() {
    let m = Machine::new(false);
    let ran = m.run(&[]);
    assert!(ran.status.success(), "{ran:?}");
    let log = m.read("gh.log");
    let calls: Vec<&str> = log.lines().collect();
    // The list names no repo, so gh is told whose it is; the rest name it.
    assert_eq!(
        calls,
        [
            "Sam api -i -X POST graphql --input -",
            "- api -i -X GET repos/sam/a/git/ref/heads/main",
            "- api -i -X GET repos/sam/a/contents/AGENTS.md?ref=c0ffee",
        ]
    );
    assert_eq!(m.read("notices"), "close house-markers\nclose house-sync\n");
}

#[test]
fn one_repo_and_status_from_the_command_line() {
    let m = Machine::new(false);
    let ran = m.run(&["sam/a"]);
    assert!(ran.status.success(), "{ran:?}");
    assert!(
        m.read("gh.log")
            .starts_with("- api -i -X GET repos/sam/a\n")
    );
    assert_eq!(m.read("notices"), "");
    // One it can't read fails, for its unit to show.
    let ran = m.run(&["sam/gone"]);
    assert_eq!(ran.status.code(), Some(1));
    let out = String::from_utf8_lossy(&ran.stdout);
    assert!(out.starts_with("house-sync: sam/gone: couldn't read it, trying again next pass (GET repos/sam/gone: HTTP 404: Not Found)"), "{out}");
    for status in [&["status"][..], &["status", "sam/a"]] {
        let ran = m.run(status);
        assert!(ran.status.success(), "{status:?}: {ran:?}");
    }
    assert_eq!(m.read("notices"), "");
    // A machine whose passes are all dry tells nobody either.
    let m = Machine::new(true);
    assert!(m.run(&[]).status.success());
    assert_eq!(m.read("notices"), "");
}

#[test]
fn what_it_cant_run() {
    let m = Machine::new(false);
    for args in [
        &["status", "sam/a", "more"][..],
        &["--help"],
        &["sam/a", "sam/b"],
    ] {
        assert_eq!(m.run(args).status.code(), Some(2), "{args:?}");
    }
    let ran = Command::new(env!("CARGO_BIN_EXE_house-sync"))
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(2));
    // A config it can't read, or with a field it doesn't know.
    let ran = Command::new(env!("CARGO_BIN_EXE_house-sync"))
        .arg(m.dir.path().join("missing.json"))
        .output()
        .unwrap();
    assert_eq!(ran.status.code(), Some(1));
    let path = m.dir.path().join("config.json");
    let mut config: serde_json::Value = serde_json::from_str(&m.read("config.json")).unwrap();
    config["dry"] = true.into();
    fs::write(&path, config.to_string()).unwrap();
    let ran = m.run(&[]);
    assert_eq!(ran.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&ran.stderr).contains("unknown field `dry`"));
    assert_eq!(m.read("gh.log"), "");
}
