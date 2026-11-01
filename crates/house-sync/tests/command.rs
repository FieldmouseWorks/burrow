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
# What gh-token would go by besides the call (account.rs).
echo "${GH_TOKEN_OWNER:--} repo=${GH_REPO:-} remotes=$(git config --get-regexp '^remote\..*\.url$')" >>"$LOG.around"
body=$(cat)
printf 'HTTP/2.0 '
case "${PAGED:-}$body" in
  'yes'*'"cursor":null'*)
    printf '200 OK\n\n{"data":{"repositoryOwner":{"repositories":{"nodes":[],"pageInfo":{"hasNextPage":true,"endCursor":"2"}}}}}'
    exit ;;
esac
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
        Machine::of(dry_run, &["Sam"])
    }

    fn of(dry_run: bool, owners: &[&str]) -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let config = serde_json::json!({
            "house": house(),
            "current": house(),
            "lock": dir.path().join("lock"),
            "owners": owners,
            "exclude": [],
            "bash": "bash",
            "notify": stand_ins().join("notify"),
            "dryRun": dry_run,
        });
        fs::write(dir.path().join("config.json"), config.to_string()).unwrap();
        Machine { dir }
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn command(&self, args: &[&str]) -> Command {
        let d = self.dir.path();
        let path = format!(
            "{}:{}",
            stand_ins().display(),
            std::env::var("PATH").unwrap()
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_house-sync"));
        command
            .arg(d.join("config.json"))
            .args(args)
            .env("PATH", path)
            .env_remove("GH_TOKEN_OWNER")
            .env_remove("GH_REPO")
            .env_remove("GIT_DIR")
            .env("LOG", d.join("gh.log"))
            .env("NOTICES", d.join("notices"));
        command
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

/// Run from another owner's checkout, with GH_REPO naming it too: each list,
/// every page of it, goes to gh as its own owner's with nothing around it
/// naming another, and the calls that name a repo are left as they were.
#[test]
fn a_list_is_the_owners_whatever_repo_it_runs_in() {
    let m = Machine::of(false, &["Sam", "Kim"]);
    let checkout = m.dir.path().join("checkout");
    fs::create_dir(&checkout).unwrap();
    let git = |args: &[&str]| {
        let ran = Command::new("git")
            .args(args)
            .current_dir(&checkout)
            .output()
            .unwrap();
        assert!(ran.status.success(), "{ran:?}");
    };
    git(&["init", "-q"]);
    git(&["remote", "add", "origin", "https://github.com/eve/x"]);
    for git_dir in [None, Some(checkout.join(".git"))] {
        let _ = fs::remove_file(m.dir.path().join("gh.log.around"));
        let mut command = m.command(&[]);
        command
            .current_dir(&checkout)
            .env("GH_REPO", "eve/x")
            .env("PAGED", "yes");
        if let Some(dir) = git_dir {
            command.env("GIT_DIR", dir);
        }
        let ran = command.output().unwrap();
        assert!(ran.status.success(), "{ran:?}");
        let around = m.read("gh.log.around");
        let lists: Vec<&str> = around.lines().filter(|l| !l.starts_with("- ")).collect();
        assert_eq!(
            lists,
            [
                "Sam repo= remotes=",
                "Sam repo= remotes=",
                "Kim repo= remotes=",
                "Kim repo= remotes=",
            ]
        );
        let rest: Vec<&str> = around.lines().filter(|l| l.starts_with("- ")).collect();
        assert!(!rest.is_empty());
        for line in rest {
            assert!(line.contains("repo=eve/x"), "{line}");
            assert!(line.contains("github.com/eve/x"), "{line}");
        }
    }
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
