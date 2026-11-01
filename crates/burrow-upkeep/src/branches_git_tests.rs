//! What git says of a delete, the git that's run, and the globs.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::OnceLock;

use super::world::sha;
use super::*;

#[test]
fn what_git_says_of_a_delete() {
    // Read whole: a push that beat it, or a branch already gone, is no
    // refusal (the next listing shows either); GitHub not answering ends
    // the look; anything else is GitHub's refusal, in its words, on one
    // line and cut short. A name git echoes (in a URL, a ref or a
    // ref-status line) that reads like an outage doesn't make one, and a
    // name that's one of git's own words doesn't hide a real one.
    let long_repo = format!("{}/{}", "o".repeat(39), "r".repeat(100));
    let x250 = "x".repeat(250);
    let refused = "refused";
    let cases: Vec<(String, String, Option<&str>)> = vec![
        ("x".into(), "! [rejected] x -> x (stale info)".into(), Some("")),
        ("x".into(), "error: unable to delete 'x': remote ref does not exist".into(), Some("")),
        ("x".into(), "remote: error: GH013: Repository rule violations found\nremote: - Cannot delete this branch".into(), Some(refused)),
        ("x".into(), "fatal: unable to access 'https://github.com/sam/r.git/': The requested URL returned error: 403".into(), Some(refused)),
        ("x".into(), "fatal: unable to access 'https://github.com/sam/r.git/': The requested URL returned error: 502".into(), None),
        ("x".into(), "fatal: unable to access 'https://github.com/sam/r.git/': Could not resolve host: github.com".into(), None),
        ("x".into(), "fatal: unable to access 'https://github.com/sam/r.git/': Failed to connect to github.com port 443: Connection timed out".into(), None),
        ("fix/timeout".into(), "remote: error: GH013: Repository rule violations found for refs/heads/fix/timeout.\nTo https://github.com/sam/r.git\n ! [remote rejected] fix/timeout (push declined due to repository rule violations)".into(), Some(refused)),
        ("x".into(), "fatal: unable to access 'https://github.com/sam/timeout.git/': The requested URL returned error: 403".into(), Some(refused)),
        ("timeout".into(), "remote: error: GH006: Protected branch update failed for refs/heads/timeout.".into(), Some(refused)),
        ("eof".into(), "To https://github.com/sam/eof.git\n ! [remote rejected] eof (protected branch hook declined)\nerror: failed to push some refs to 'https://github.com/sam/eof.git'".into(), Some(refused)),
        ("error".into(), "fatal: unable to access 'https://github.com/sam/r.git/': The requested URL returned error: 502".into(), None),
        ("502".into(), "error: RPC failed; HTTP 502 curl 22 The requested URL returned error: 502".into(), None),
        ("host".into(), "fatal: unable to access 'https://github.com/sam/host.git/': Could not resolve host: github.com".into(), None),
        ("timeout".into(), "fatal: unable to access 'https://github.com/sam/r.git/': Failed to connect to github.com port 443: Connection timed out".into(), None),
        ("x".into(), format!("fatal: unable to access 'https://github.com/{long_repo}.git/': The requested URL returned error: 502"), None),
        (x250.clone(), format!("! [rejected] {x250} -> {x250} (stale info)"), Some("")),
        ("x".into(), format!("remote: error: GH013: {x250}"), Some(refused)),
    ];
    for (name, said, want) in cases {
        let got = verdict(
            &name,
            &Pushed {
                code: Some(1),
                said: said.clone(),
            },
        );
        let want = want.map(|w| match w {
            "refused" => said
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(200)
                .collect(),
            w => w.to_string(),
        });
        assert_eq!(got.ok(), want, "{said}");
    }
    let done = Pushed {
        code: Some(0),
        said: "remote: error: anything".into(),
    };
    assert_eq!(verdict("x", &done), Ok(String::new()));
    let quiet = Pushed {
        code: Some(128),
        said: String::new(),
    };
    assert_eq!(verdict("x", &quiet), Ok("git exited 128".into()));
}

/// A git that notes how it was run (`log` beside it) and answers a push as
/// `says` there tells it: the exit code, then what it prints. With
/// `init-fails` beside it, its init fails.
fn git() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("git");
        let script = "#!/bin/sh\n\
            here=$(dirname \"$0\")\n\
            printf '%s\\n' \"$*\" >>\"$here/log\"\n\
            case \"$*\" in init*) [ -e \"$here/init-fails\" ] && exit 3; mkdir -p \"$4\"; exit 0;; esac\n\
            [ -d \"$2\" ] || exit 9\n\
            code=$(head -n1 \"$here/says\")\n\
            tail -n +2 \"$here/says\" >&2\n\
            exit \"$code\"\n";
        std::fs::write(&bin, script).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        d
    })
    .path()
}

#[test]
fn git_deletes_from_a_bare_repo_of_its_own_with_gh_as_its_only_credential_helper() {
    let dir = git();
    std::fs::write(
        dir.join("says"),
        "1\nremote: error: GH013: Repository rule violations\n",
    )
    .unwrap();
    let mut g = Git {
        bin: dir.join("git"),
    };
    let pushed = g.delete("sam/r", "feat/x", &sha("1")).unwrap();
    assert_eq!(pushed.code, Some(1));
    assert_eq!(
        pushed.said,
        "remote: error: GH013: Repository rule violations\n"
    );
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 2, "{log}");
    let init = lines[0].strip_prefix("init -q --bare ").expect(lines[0]);
    let want = format!(
        "-C {init} -c credential.helper= -c credential.helper=!gh auth git-credential push -q \
         --force-with-lease=refs/heads/feat/x:{} https://github.com/sam/r.git :refs/heads/feat/x",
        sha("1")
    );
    assert_eq!(lines[1], want);
    // Its dir goes with it.
    assert!(!Path::new(init).exists());
    // Nor is a push tried from a dir git couldn't set up.
    std::fs::write(dir.join("init-fails"), "").unwrap();
    let failed = g.delete("sam/r", "feat/x", &sha("1")).err();
    std::fs::remove_file(dir.join("init-fails")).unwrap();
    assert!(
        failed.as_ref().is_some_and(|e| e.starts_with("git init: ")),
        "{failed:?}"
    );
    let log = std::fs::read_to_string(dir.join("log")).unwrap();
    assert_eq!(log.lines().count(), 3, "{log}");
    // git not there to run fails the look, and says so.
    let mut none = Git {
        bin: dir.join("no-git"),
    };
    assert!(none.delete("sam/r", "x", &sha("1")).is_err());
}

#[test]
fn a_glob_is_as_fnmatch_has_it() {
    for (name, pat) in [
        ("gh-pages", "gh-pages"),
        ("release/1.2", "release/*"),
        ("release/a/b", "release/*"),
        ("conary/train/7", "conary/*"),
        ("v1", "v?"),
        ("v1", "v[0-9]"),
        ("vx", "v[!0-9]"),
        ("a]", "a[]]"),
        ("a[", "a["),
        ("a-", "a[x-]"),
        ("", "*"),
    ] {
        assert!(glob(name, pat), "{name} {pat}");
    }
    for (name, pat) in [
        ("gh-pages-old", "gh-pages"),
        ("releases", "release/*"),
        ("v12", "v?"),
        ("v1", "v[!0-9]"),
        ("Release/1", "release/*"),
        ("a", "a["),
    ] {
        assert!(!glob(name, pat), "{name} {pat}");
    }
}
