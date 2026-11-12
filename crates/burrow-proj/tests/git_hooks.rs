//! Real hooks on a scratch worktree and a remote whose default is not main.
use std::{
    fs,
    os::unix::fs::symlink,
    path::Path,
    process::{Command, Output},
};

fn git(dir: &Path, hooks: &Path, args: &[&str]) -> Output {
    git_with_hash(dir, hooks, args, "sha1")
}

fn git_with_hash(dir: &Path, hooks: &Path, args: &[&str], hash: &str) -> Output {
    Command::new("git")
        .env("GIT_DEFAULT_HASH", hash)
        .arg(if dir.join("HEAD").exists() {
            "--git-dir"
        } else {
            "-C"
        })
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_COUNT", "5")
        .env("GIT_CONFIG_KEY_4", "safe.bareRepository")
        .env("GIT_CONFIG_VALUE_4", "explicit")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", hooks)
        .env("GIT_CONFIG_KEY_1", "user.name")
        .env("GIT_CONFIG_VALUE_1", "Worker")
        .env("GIT_CONFIG_KEY_2", "user.email")
        .env("GIT_CONFIG_VALUE_2", "worker@example.test")
        .env("GIT_CONFIG_KEY_3", "commit.gpgsign")
        .env("GIT_CONFIG_VALUE_3", "false")
        .output()
        .unwrap()
}
fn succeeds(out: Output) -> String {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn worktree_commits_are_marked_and_default_branch_rewrites_and_deletes_are_refused() {
    let d = tempfile::tempdir().unwrap();
    let hooks = d.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    for name in ["commit-msg", "pre-push"] {
        symlink(env!("CARGO_BIN_EXE_burrow-proj-git-hook"), hooks.join(name)).unwrap();
    }
    let clone = d.path().join("clone");
    fs::create_dir(&clone).unwrap();
    let run = |dir: &Path, args: &[&str]| git(dir, &hooks, args);
    succeeds(run(&clone, &["init", "-b", "trunk"]));
    succeeds(run(&clone, &["commit", "--allow-empty", "-m", "initial"]));
    let initial = succeeds(run(&clone, &["rev-parse", "HEAD"]));
    let remote = d.path().join("remote");
    succeeds(run(
        d.path(),
        &[
            "clone",
            "--bare",
            clone.to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    ));
    // Without the hook, this bare remote permits deleting its default branch.
    succeeds(run(
        &remote,
        &["config", "receive.denyDeleteCurrent", "false"],
    ));
    succeeds(run(
        &clone,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    ));
    let tree = d.path().join("scratch");
    succeeds(run(
        &clone,
        &["worktree", "add", "-b", "work", tree.to_str().unwrap()],
    ));
    succeeds(run(&tree, &["commit", "--allow-empty", "-m", "changed"]));
    succeeds(run(
        &tree,
        &["commit", "--amend", "--allow-empty", "--no-edit"],
    ));
    let message = succeeds(run(&tree, &["log", "-1", "--format=%B"]));
    assert_eq!(message.matches("Engine: codex").count(), 1);
    succeeds(run(&tree, &["push", "origin", "HEAD:work"]));
    succeeds(run(&tree, &["push", "origin", "HEAD:trunk"])); // fast-forward is allowed
    let tip = succeeds(run(&remote, &["rev-parse", "trunk"]));
    succeeds(run(&tree, &["reset", "--hard", &initial]));
    for args in [
        ["push", "--force", "origin", "HEAD:trunk"].as_slice(),
        &["push", "origin", ":trunk"],
    ] {
        let refused = run(&tree, args);
        assert!(!refused.status.success());
        assert!(
            String::from_utf8_lossy(&refused.stderr)
                .contains("cannot delete or force-push refs/heads/trunk")
        );
        assert_eq!(succeeds(run(&remote, &["rev-parse", "trunk"])), tip);
    }
}

/// A scratch repo with the hooks on, its remote with one commit of
/// someone else's on its default branch (unmarked, made with no hooks),
/// fetched: the job's history to build on.
struct Repo {
    _dir: tempfile::TempDir,
    hooks: std::path::PathBuf,
    work: std::path::PathBuf,
    remote: std::path::PathBuf,
}

impl Repo {
    fn new() -> Repo {
        Self::with_format("sha1")
    }

    fn with_format(format: &str) -> Repo {
        let d = tempfile::tempdir().unwrap();
        let hooks = d.path().join("hooks");
        fs::create_dir(&hooks).unwrap();
        for name in ["commit-msg", "pre-push"] {
            symlink(env!("CARGO_BIN_EXE_burrow-proj-git-hook"), hooks.join(name)).unwrap();
        }
        let none = d.path().join("no-hooks");
        fs::create_dir(&none).unwrap();
        let theirs = d.path().join("theirs");
        let remote = d.path().join("remote");
        let work = d.path().join("work");
        succeeds(git(
            d.path(),
            &none,
            &[
                "init",
                &format!("--object-format={format}"),
                "-q",
                "-b",
                "main",
                "theirs",
            ],
        ));
        succeeds(git(
            &theirs,
            &none,
            &["commit", "--allow-empty", "-m", "theirs"],
        ));
        let (theirs, remote_s) = (theirs.to_str().unwrap(), remote.to_str().unwrap());
        succeeds(git(
            d.path(),
            &none,
            &["clone", "-q", "--bare", theirs, remote_s],
        ));
        succeeds(git(d.path(), &none, &["clone", "-q", remote_s, "work"]));
        Repo {
            hooks,
            work,
            remote,
            _dir: d,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        git(&self.work, &self.hooks, args)
    }

    fn ok(&self, args: &[&str]) -> String {
        succeeds(self.run(args))
    }

    /// Pushes HEAD to `branch`: the remote's branch after, or what refused.
    fn push(&self, branch: &str) -> Result<String, String> {
        let out = self.run(&["push", "origin", &format!("HEAD:refs/heads/{branch}")]);
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).into_owned());
        }
        Ok(succeeds(git(
            &self.remote,
            &self.hooks,
            &["rev-parse", branch],
        )))
    }
}

/// The steward's reading of a stored commit message: `git
/// interpret-trailers --parse`, case-blind (crates/burrow-upkeep/src/commits.rs).
fn steward_sees_mark(repo: &Repo, commit: &str) -> bool {
    let message = repo.ok(&["log", "-1", "--format=%B", commit]);
    let mut child = Command::new("git")
        .args(["interpret-trailers", "--parse"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(&mut child.stdin.take().unwrap(), message.as_bytes()).unwrap();
    let out = succeeds(child.wait_with_output().unwrap());
    out.to_lowercase().lines().any(|l| l == "engine: codex")
}

/// Whatever message and cleanup a commit is made with, the one stored
/// carries the mark where the steward reads it, or there's no commit.
#[test]
fn every_stored_message_carries_the_mark_or_there_is_no_commit() {
    let repo = Repo::new();
    let file = repo.work.join(".git").join("message");
    fs::write(&file, "subject\n\nbody\n# a comment\n").unwrap();
    let file = file.to_str().unwrap();
    for args in [
        ["commit", "--allow-empty", "-m", ""].as_slice(),
        &["commit", "--allow-empty", "--allow-empty-message", "-m", ""],
        &["commit", "--allow-empty", "-m", "   \n\t"],
        &["commit", "--allow-empty", "--cleanup=strip", "-m", "# c"],
        &["commit", "--allow-empty", "--cleanup=verbatim", "-m", ""],
        &[
            "commit",
            "--allow-empty",
            "--cleanup=whitespace",
            "-m",
            "\n\n",
        ],
        &["commit", "--allow-empty", "--cleanup=scissors", "-m", ""],
        &[
            "-c",
            "core.commentChar=E",
            "commit",
            "--allow-empty",
            "--cleanup=strip",
            "-m",
            "x",
        ],
        &["commit", "--allow-empty", "-m", "subject"],
        &["commit", "--allow-empty", "--cleanup=verbatim", "-m", "# c"],
        &["commit", "--allow-empty", "--cleanup=strip", "-F", file],
        &["commit", "--allow-empty", "--cleanup=verbatim", "-F", file],
        &[
            "commit",
            "--allow-empty",
            "--cleanup=whitespace",
            "-F",
            file,
        ],
        &["commit", "--allow-empty", "--cleanup=scissors", "-F", file],
    ] {
        let before = repo.ok(&["rev-parse", "HEAD"]);
        let made = repo.run(args).status.success();
        let head = repo.ok(&["rev-parse", "HEAD"]);
        assert_eq!(made, head != before, "{args:?}");
        if made {
            assert!(steward_sees_mark(&repo, &head), "{args:?}: unmarked");
        }
    }
}

/// However a commit is made past commit-msg, it doesn't leave unmarked:
/// pre-push reads each commit new to the remote.
#[test]
fn a_commit_made_past_commit_msg_isnt_pushed() {
    let repo = Repo::new();
    let base = repo.ok(&["rev-parse", "HEAD"]);
    // Plumbing: a commit object and a ref, no commit-msg.
    let tree = repo.ok(&["rev-parse", "HEAD^{tree}"]);
    let object = format!(
        "tree {tree}\nparent {base}\nauthor W <w@x> 1 +0000\ncommitter W <w@x> 1 +0000\n\nplumbed\n"
    );
    fs::write(repo.work.join(".git").join("object"), object).unwrap();
    let plumbed = repo.ok(&["hash-object", "-t", "commit", "-w", ".git/object"]);
    let stashed = {
        fs::write(repo.work.join("file"), "x").unwrap();
        repo.ok(&["add", "file"]);
        repo.ok(&["stash"]);
        repo.ok(&["rev-parse", "stash@{0}"])
    };
    let hooks_off = {
        repo.ok(&["commit", "--allow-empty", "--no-verify", "-m", "past it"]);
        repo.ok(&["rev-parse", "HEAD"])
    };
    let aliased = {
        repo.ok(&["reset", "-q", "--hard", &base]);
        repo.ok(&[
            "-c",
            "alias.c=commit --no-verify",
            "c",
            "--allow-empty",
            "-m",
            "alias",
        ]);
        repo.ok(&["rev-parse", "HEAD"])
    };
    let configured = {
        repo.ok(&["reset", "-q", "--hard", &base]);
        repo.ok(&[
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--allow-empty",
            "-m",
            "cfg",
        ]);
        repo.ok(&["rev-parse", "HEAD"])
    };
    // Under a marked commit, or alone.
    for commit in [plumbed, stashed, hooks_off, aliased, configured] {
        for on_top in [false, true] {
            repo.ok(&["update-ref", "HEAD", &commit]);
            if on_top {
                repo.ok(&["commit", "--allow-empty", "-m", "marked"]);
            }
            let refused = repo.push("work").unwrap_err();
            assert!(refused.contains(&commit), "{refused}");
            assert!(refused.contains("no Engine: codex trailer"), "{refused}");
        }
    }
}

/// History the remote already has isn't the job's to mark: a marked
/// commit on it, and a merge with the remote's own, push.
#[test]
fn what_the_remote_has_is_not_checked() {
    for format in ["sha1", "sha256"] {
        published_history(format);
    }
}

fn published_history(format: &str) {
    let repo = Repo::with_format(format);
    repo.ok(&["commit", "--allow-empty", "-m", "mine"]);
    let mine = repo.ok(&["rev-parse", "HEAD"]);
    assert_eq!(repo.push("work"), Ok(mine.clone()));
    // Someone else's unmarked commit lands on main.
    let other = repo.work.with_file_name("other");
    let none = repo.work.with_file_name("no-hooks");
    let remote = repo.remote.to_str().unwrap();
    succeeds(git(
        repo.work.parent().unwrap(),
        &none,
        &["clone", "-q", remote, "other"],
    ));
    succeeds(git(
        &other,
        &none,
        &["commit", "--allow-empty", "-m", "theirs too"],
    ));
    succeeds(git(&other, &none, &["push", "-q", "origin", "main"]));
    repo.ok(&["fetch", "-q", "origin"]);
    repo.ok(&["merge", "--no-ff", "--no-edit", "origin/main"]);
    let merge = repo.ok(&["rev-parse", "HEAD"]);
    assert!(steward_sees_mark(&repo, &merge));
    // Main moves again after our fetch and merge. Its advertised tip is
    // absent locally, but the merged parent is still published history.
    succeeds(git(
        &other,
        &none,
        &["commit", "--allow-empty", "-m", "later"],
    ));
    succeeds(git(&other, &none, &["push", "-q", "origin", "main"]));
    succeeds(git_with_hash(
        &repo.work,
        &repo.hooks,
        &["push", "origin", "HEAD:refs/heads/work"],
        if format == "sha1" { "sha256" } else { "sha1" },
    ));
    assert_eq!(repo.ok(&["rev-parse", "origin/work"]), merge);
    // Their commit, already there, onto a new branch.
    repo.ok(&["reset", "-q", "--hard", "origin/main"]);
    let theirs = repo.ok(&["rev-parse", "HEAD"]);
    assert_eq!(repo.push("copy"), Ok(theirs));
}

/// Nothing the job can write here speaks for the remote or for a commit:
/// a remote-tracking ref naming its unmarked commit, a replace ref showing
/// a marked one in its place, trailer settings that read another trailer
/// as the mark, and settings that send the hook's question about what the
/// remote has to a remote of the job's that has the commit. Each push is
/// refused, and the commit isn't on the remote.
#[test]
fn nothing_the_job_writes_exempts_or_marks_a_commit() {
    let repo = Repo::new();
    let refused = |args: &[&str], commit: &str| {
        let out = repo.run(args);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} pushed");
        assert!(stderr.contains(commit), "{args:?}: {stderr}");
        assert!(
            stderr.contains("no Engine: codex trailer"),
            "{args:?}: {stderr}"
        );
        let on_remote = git(&repo.remote, &repo.hooks, &["cat-file", "-e", commit]);
        assert!(!on_remote.status.success(), "{args:?}: {commit} is there");
    };
    let push = ["push", "origin", "HEAD:refs/heads/work"];
    repo.ok(&["commit", "--allow-empty", "--no-verify", "-m", "past it"]);
    let unmarked = repo.ok(&["rev-parse", "HEAD"]);

    // A remote-tracking ref of the job's.
    repo.ok(&["update-ref", "refs/remotes/origin/unmarked", &unmarked]);
    refused(&push, &unmarked);
    repo.ok(&["update-ref", "-d", "refs/remotes/origin/unmarked"]);

    // A marked commit shown in its place.
    let tree = repo.ok(&["rev-parse", "HEAD^{tree}"]);
    let parent = repo.ok(&["rev-parse", "HEAD^"]);
    let object = format!(
        "tree {tree}\nparent {parent}\nauthor W <w@x> 1 +0000\ncommitter W <w@x> 1 +0000\n\nstand-in\n\nEngine: codex\n"
    );
    fs::write(repo.work.join(".git").join("object"), object).unwrap();
    let stand_in = repo.ok(&["hash-object", "-t", "commit", "-w", ".git/object"]);
    let replace = format!("refs/replace/{unmarked}");
    repo.ok(&["update-ref", &replace, &stand_in]);
    assert!(
        repo.ok(&["log", "-1", "--format=%B"])
            .contains("Engine: codex")
    );
    refused(&push, &unmarked);
    repo.ok(&["update-ref", "-d", &replace]);

    // Another trailer read as the mark, by `git -c` or the repo's config.
    repo.ok(&["reset", "-q", "--hard", &parent]);
    repo.ok(&[
        "commit",
        "--allow-empty",
        "--no-verify",
        "-m",
        "subject\n\nfake: codex",
    ]);
    let fake = repo.ok(&["rev-parse", "HEAD"]);
    let mut with_c = vec!["-c", "trailer.fake.key=Engine"];
    with_c.extend(push);
    refused(&with_c, &fake);
    repo.ok(&["config", "trailer.fake.key", "Engine"]);
    refused(&push, &fake);
    repo.ok(&["config", "--unset", "trailer.fake.key"]);

    // The question about what the remote has, sent to the job's own remote
    // that has the commit, while the push goes to the real one: by the
    // repo's config, `git -c`, or a moved HOME's.
    let decoy = repo.work.with_file_name("decoy.git");
    let none = repo.work.with_file_name("no-hooks");
    let (decoy_s, remote_s) = (decoy.to_str().unwrap(), repo.remote.to_str().unwrap());
    succeeds(git(
        repo.work.parent().unwrap(),
        &none,
        &["clone", "-q", "--bare", remote_s, decoy_s],
    ));
    succeeds(git(
        &repo.work,
        &none,
        &["push", "-q", decoy_s, "HEAD:refs/heads/x"],
    ));
    let fetch = format!("url.{decoy_s}.insteadOf");
    let send = format!("url.{remote_s}.pushInsteadOf");
    repo.ok(&["config", &fetch, remote_s]);
    repo.ok(&["config", &send, remote_s]);
    refused(&push, &fake);
    repo.ok(&["config", "--unset", &fetch]);
    repo.ok(&["config", "--unset", &send]);
    let (fetch_c, send_c) = (format!("{fetch}={remote_s}"), format!("{send}={remote_s}"));
    let mut with_c = vec!["-c", &fetch_c, "-c", &send_c];
    with_c.extend(push);
    refused(&with_c, &fake);
    let home = repo.work.with_file_name("home");
    fs::create_dir(&home).unwrap();
    fs::write(
        home.join(".gitconfig"),
        format!("[url \"{decoy_s}\"]\n\tinsteadOf = {remote_s}\n[url \"{remote_s}\"]\n\tpushInsteadOf = {remote_s}\n"),
    )
    .unwrap();
    let out = Command::new("git")
        .arg("-C")
        .arg(&repo.work)
        .args(push)
        .env("HOME", &home)
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", &repo.hooks)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success() && stderr.contains(&fake), "{stderr}");

    // Unchanged, the same commit marked properly pushes.
    repo.ok(&["reset", "-q", "--hard", &parent]);
    repo.ok(&["commit", "--allow-empty", "-m", "marked"]);
    let marked = repo.ok(&["rev-parse", "HEAD"]);
    assert_eq!(repo.push("work"), Ok(marked));
}

/// A remote given as a relative path, configured or on the command line,
/// is the one git pushes to: from a checkout, a subdirectory of it and a
/// worktree, a first push to an empty remote, a fast-forward, a rewrite
/// and a delete go through, and the default branch's rewrite and delete
/// are refused because the hook read that remote's HEAD.
#[test]
fn a_relative_remote_is_the_one_pushed_to() {
    let repo = Repo::new();
    let base = repo.work.parent().unwrap();
    let none = base.join("no-hooks");
    succeeds(git(
        &repo.remote,
        &none,
        &["config", "receive.denyDeleteCurrent", "false"],
    ));
    succeeds(git(
        base,
        &none,
        &["init", "-q", "--bare", "-b", "main", "empty"],
    ));
    repo.ok(&["remote", "set-url", "origin", "../remote"]);
    repo.ok(&["remote", "add", "empty", "../empty"]);
    fs::create_dir(repo.work.join("sub")).unwrap();
    let tree = base.join("tree");
    repo.ok(&[
        "worktree",
        "add",
        "-q",
        "-b",
        "tree",
        tree.to_str().unwrap(),
    ]);
    let initial = repo.ok(&["rev-parse", "HEAD"]);
    let refused = |dir: &Path, args: &[&str], what: &str| {
        let out = git(dir, &repo.hooks, args);
        assert!(!out.status.success(), "{args:?} went through");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains(what), "{args:?}: {stderr}");
    };
    // An empty remote's first push (of the job's own history alone), then
    // its default kept.
    repo.ok(&["checkout", "-q", "--orphan", "fresh"]);
    repo.ok(&["commit", "--allow-empty", "-m", "first"]);
    repo.ok(&["push", "-q", "empty", "HEAD:main"]);
    repo.ok(&["tag", "orphan"]);
    repo.ok(&["checkout", "-q", "main"]);
    refused(
        &repo.work,
        &["push", "empty", ":main"],
        "cannot delete or force-push refs/heads/main",
    );
    let sub = repo.work.join("sub");
    for (dir, remote) in [
        (&repo.work, "origin"),
        (&repo.work, "../remote"),
        (&sub, "origin"),
        (&tree, "origin"),
        (&tree, "../remote"),
    ] {
        let run = |args: &[&str]| succeeds(git(dir, &repo.hooks, args));
        run(&["reset", "-q", "--hard", &initial]);
        run(&["commit", "--allow-empty", "-m", "one"]);
        let one = run(&["rev-parse", "HEAD"]);
        run(&["push", "-q", remote, "HEAD:refs/heads/a"]);
        run(&["commit", "--allow-empty", "-m", "two"]);
        run(&["push", "-q", remote, "HEAD:refs/heads/a"]);
        run(&["reset", "-q", "--hard", &one]);
        run(&["push", "-q", "--force", remote, "HEAD:refs/heads/a"]);
        run(&["push", "-q", remote, ":refs/heads/a"]);
        refused(
            dir,
            &["push", "--force", remote, "orphan:main"],
            "cannot delete or force-push refs/heads/main",
        );
        refused(
            dir,
            &["push", remote, ":main"],
            "cannot delete or force-push refs/heads/main",
        );
    }
    assert_eq!(
        succeeds(git(&repo.remote, &none, &["rev-parse", "main"])),
        initial
    );
}

/// An explicit source repository must survive clearing the hook environment.
#[test]
fn explicit_git_dir_does_not_check_the_current_directory_instead() {
    let repo = Repo::new();
    let other = repo.work.with_file_name("selected");
    let none = repo.work.with_file_name("no-hooks");
    succeeds(git(
        repo.work.parent().unwrap(),
        &none,
        &["clone", "-q", repo.remote.to_str().unwrap(), "selected"],
    ));
    succeeds(git(
        &other,
        &none,
        &["commit", "--allow-empty", "-m", "unmarked"],
    ));
    let args = [
        format!("--git-dir={}", other.join(".git").display()),
        format!("--work-tree={}", other.display()),
    ];
    let output = repo.run(&[
        &args[0],
        &args[1],
        "push",
        repo.remote.to_str().unwrap(),
        "HEAD:work",
    ]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no Engine: codex trailer"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "a test moving its own fixture, not writing state"
)]
fn source_environment_object_store_and_alternates_are_preserved() {
    let repo = Repo::new();
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let objects = repo.work.with_file_name(std::ffi::OsString::from_vec(
        b"objects\n\xff\t\"\\".to_vec(),
    ));
    fs::create_dir(&objects).unwrap();
    let alternate = objects.with_file_name(std::ffi::OsString::from_vec(
        b"alternate\n\xff\t\"\\".to_vec(),
    ));
    fs::rename(repo.work.join(".git/objects"), &alternate).unwrap();
    let quoted_alternate = format!(
        "\"{}\"",
        alternate
            .as_os_str()
            .as_bytes()
            .iter()
            .map(|b| format!("\\{b:03o}"))
            .collect::<String>()
    );
    let run = |args: &[&str], hooks: &Path| {
        Command::new("git")
            .current_dir(&repo.work)
            .env("GIT_DIR", repo.work.join(".git"))
            .env("GIT_WORK_TREE", &repo.work)
            .env("GIT_COMMON_DIR", repo.work.join(".git"))
            .env("GIT_OBJECT_DIRECTORY", &objects)
            .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &quoted_alternate)
            .args([
                "-c",
                &format!("core.hooksPath={}", hooks.display()),
                "-c",
                "user.name=Worker",
                "-c",
                "user.email=worker@example.test",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap()
    };
    succeeds(run(
        &["commit", "--allow-empty", "-m", "marked"],
        &repo.hooks,
    ));
    succeeds(run(&["push", "origin", "HEAD:work"], &repo.hooks));
    let none = repo.work.with_file_name("no-hooks");
    succeeds(run(&["commit", "--allow-empty", "-m", "unmarked"], &none));
    let refused = run(&["push", "origin", "HEAD:work"], &repo.hooks);
    assert!(!refused.status.success());
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("no Engine: codex trailer"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

#[test]
fn noncommit_objects_and_nested_tags_can_be_pushed_together() {
    let repo = Repo::new();
    fs::write(repo.work.join("blob"), "contents").unwrap();
    let blob = repo.ok(&["hash-object", "-w", "blob"]);
    let tree = repo.ok(&["rev-parse", "HEAD^{tree}"]);
    repo.ok(&["tag", "blob", &blob]);
    repo.ok(&["tag", "tree", &tree]);
    repo.ok(&["tag", "-a", "annotated-blob", &blob, "-m", "blob"]);
    repo.ok(&["tag", "-a", "nested", "annotated-blob", "-m", "nested"]);
    repo.ok(&["commit", "--allow-empty", "-m", "marked"]);
    repo.ok(&["tag", "-a", "commit", "HEAD", "-m", "commit"]);
    repo.ok(&["push", "origin", "HEAD:work", "--tags"]);
    let none = repo.work.with_file_name("no-hooks");
    succeeds(git(
        &repo.work,
        &none,
        &["commit", "--allow-empty", "-m", "unmarked"],
    ));
    repo.ok(&["tag", "-a", "unmarked-commit", "HEAD", "-m", "commit"]);
    repo.ok(&[
        "tag",
        "-a",
        "nested-unmarked",
        "unmarked-commit",
        "-m",
        "nested",
    ]);
    let refused = repo.run(&["push", "origin", "--tags"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("no Engine: codex trailer"));
}

#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "a test moving its own fixture, not writing state"
)]
fn checkout_paths_preserve_arbitrary_bytes() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let mut repo = Repo::new();
    let work = repo
        .work
        .with_file_name(OsString::from_vec(b"checkout\n\xff\"\\".to_vec()));
    fs::rename(&repo.work, &work).unwrap();
    repo.work = work;
    repo.ok(&["commit", "--allow-empty", "-m", "marked"]);
    repo.push("work").unwrap();
}
