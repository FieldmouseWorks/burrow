//! Where a push runs, as the guard follows a `cd` to it
//! (FieldmouseWorks/burrow#1329): git is asked about that place, the one
//! the shell is in when the push runs, and a `cd` the guard can't follow
//! leaves the place unread, so the push is refused as one from
//! `git -C "$DIR"` is.

use std::path::Path;
use std::time::SystemTime;

use burrow_guard::Guard;
use burrow_guard::config::Config;
use tempfile::TempDir;

mod common;

use common::{bash, clone_on, git};

/// The refusal for a push its remote's configuration forces.
const CONFIGURED: &str = "Push its remote's git configuration forces";
/// The refusal for a push the guard can't read.
const UNREAD: &str = "The guard can't read all of this push";
/// The refusal for a force-push with no lease on what its pusher saw.
const UNLEASED: &str = "Force-push without a lease";

fn config() -> Config {
    Config {
        git: "git".into(),
        zfs: "/no-such-zfs".into(),
        snapshot_datasets: Vec::new(),
        ask_after_seconds: 8,
    }
}

/// Which refusal the guard gives `cmd` run in `cwd`, by its first words.
fn refusal(cmd: &str, cwd: &Path) -> Option<&'static str> {
    let config = config();
    let guard = Guard {
        config: &config,
        mark: "/nonexistent/mark".into(),
    };
    let said = guard.refusal(&bash(cmd, cwd))?;
    Some(
        [CONFIGURED, UNREAD, UNLEASED]
            .into_iter()
            .find(|r| said.starts_with(r))
            .unwrap_or_else(|| panic!("{cmd}: {said}")),
    )
}

/// What the guard asks of `cmd` run in `cwd`.
fn question(cmd: &str, cwd: &Path) -> Option<String> {
    let config = config();
    let guard = Guard {
        config: &config,
        mark: "/nonexistent/mark".into(),
    };
    guard.check(&bash(cmd, cwd), SystemTime::now())
}

fn assert_each(cwd: &Path, cmds: &[String], said: Option<&str>) {
    for cmd in cmds {
        assert_eq!(refusal(cmd, cwd), said, "{cmd}");
    }
}

/// A repository on `pr` whose remote is configured to force it.
fn forcing() -> TempDir {
    let dir = clone_on("pr");
    git(
        dir.path(),
        &[
            "config",
            "remote.origin.push",
            "+refs/heads/pr:refs/heads/pr",
        ],
    );
    dir
}

/// Each of `cmds` with `OTHER` spelled out as `other`.
fn at(other: &Path, cmds: &[&str]) -> Vec<String> {
    let other = other.to_str().unwrap();
    cmds.iter().map(|c| c.replace("OTHER", other)).collect()
}

#[test]
fn a_push_after_a_cd_is_read_where_the_cd_went() {
    let here = clone_on("pr");
    let other = forcing();
    let (cwd, o) = (here.path(), other.path());
    let parent = o.parent().unwrap().to_str().unwrap();
    let name = o.file_name().unwrap().to_str().unwrap();
    let mut cmds = at(
        o,
        &[
            // As `-C` is.
            "git -C OTHER push origin pr",
            "cd OTHER && git push origin pr",
            "cd OTHER || exit 1; git push origin pr",
            "cd OTHER || exit\ngit push origin pr",
            "git fetch && cd OTHER || exit 1; git push origin pr",
            "cd OTHER && git fetch || exit 1; git push origin pr",
            "cd -- OTHER && git push origin pr",
            "cd 'OTHER' && git push origin pr",
            "pushd OTHER && git push origin pr",
            "cd OTHER/.git/.. && git push origin pr",
            "cd OTHER && git -C . push origin pr",
            "cd OTHER || exit 1; git push origin pr",
            "cd OTHER && echo \"$(git push origin pr)\"",
            "git fetch && cd OTHER && git push origin pr",
            "git fetch &&\n  cd OTHER &&\n  git push origin pr",
            "git fetch && cd OTHER && (git push origin pr)",
            // A subshell's own, and a shell's.
            "(cd OTHER && git push origin pr)",
            "sh -c 'cd OTHER && git push origin pr'",
            "sh -c 'cd \"$1\" && git push origin pr' _ OTHER",
            // A here-document's script is one, line after line.
            "bash <<'EOF'\ncd OTHER || exit 1\ngit push origin pr\nEOF",
            "bash <<'EOF'\ntrue &&\n  cd OTHER &&\n  git push origin pr\nEOF",
            // Parentheses that open no function.
            "files=(a b); cd OTHER && git push origin pr",
            "[[ x =~ (a|b) ]]; cd OTHER && git push origin pr",
            // A place spelled out from the root reads it again.
            "cd \"$D\" && cd OTHER && git push origin pr",
            "cd \"$D\" && git -C OTHER push origin pr",
            "cd /nowhere && cd OTHER && git push origin pr",
        ],
    );
    cmds.push(format!("cd {parent} && cd {name} && git push origin pr"));
    cmds.push(format!(
        "cd {parent} || exit; cd ./{name}/ && git push origin pr"
    ));
    assert_each(cwd, &cmds, Some(CONFIGURED));

    // A cd away from it, or one that ends with its shell, leaves it.
    let h = cwd.to_str().unwrap();
    let mut cmds = at(
        o,
        &[
            "(cd OTHER); git push origin pr",
            "(cd OTHER && make); git push origin pr",
            "D=$(cd OTHER && pwd); git push origin pr",
            "bash -c 'cd OTHER'; git push origin pr",
            "git commit -m 'cd OTHER' && git push origin pr",
            "echo cd OTHER; git push origin pr",
            "env cd OTHER; git push origin pr",
            // Prose read as a script moves nothing, and holds no loop or
            // function for a cd to spoil.
            "# cd OTHER (the worktree)\ngit push origin pr",
            "cd OTHER || exit # (the worktree)\ncd .. && git push origin pr",
            "git push origin pr && gh pr create --body \"$(cat <<'EOF'\n**Summary** of it (short)\n* cd OTHER\nfor a while (until done)\nEOF\n)\"",
            "git commit -F - <<'EOF'\nplace: cd OTHER first (see #1)\n\n`cd` it { now }\nEOF\ngit push origin pr",
        ],
    );
    cmds.push(format!("cd {h} && git push origin pr"));
    assert_each(cwd, &cmds, None);
    assert_each(
        o,
        &[
            format!("cd {h} && git push origin pr"),
            format!("pushd {h} && git push origin pr"),
        ],
        None,
    );
}

#[test]
fn a_cd_the_guard_cant_follow_leaves_the_place_unread() {
    let here = clone_on("pr");
    let other = forcing();
    let cwd = here.path();
    let cmds = at(
        other.path(),
        &[
            // Where it goes isn't spelled out.
            "D=OTHER; cd \"$D\" && git push origin pr",
            "cd $D && git push origin pr",
            "cd $(echo OTHER) && git push origin pr",
            "cd OTHER* && git push origin pr",
            "cd - && git push origin pr",
            "cd && git push origin pr",
            "cd ~ && git push origin pr",
            "cd ~/x && git push origin pr",
            "cd -P OTHER && git push origin pr",
            "cd OTHER x && git push origin pr",
            "cd '' && git push origin pr",
            "pushd +1 && git push origin pr",
            "popd && git push origin pr",
            "CDPATH=/; cd x && git push origin pr",
            // A relative cd from an unread place stays unread.
            "cd \"$D\" && cd sub && git push origin pr",
            "cd \"$D\" && git -C sub push origin pr",
            // It may have failed, and the shell gone on where it was: a
            // directory missing, even one `..` steps back out of.
            "cd OTHER; git push origin pr",
            "cd OTHER\ngit push origin pr",
            "cd OTHER/missing/..; git push origin pr",
            "cd OTHER && git fetch; git push origin pr",
            "cd OTHER || echo no; git push origin pr",
            "cd OTHER || (exit 1); git push origin pr",
            // `return` outside a function only fails, and an `exit` in the
            // background or a pipeline ends only its own subshell.
            "cd OTHER || return; git push origin pr",
            "cd OTHER/missing/.. || return; git push origin pr",
            "cd OTHER || exit 1 & wait; git push origin pr",
            "cd OTHER || exit 1 && true & wait; git push origin pr",
            "cd OTHER || exit 1 | cat; git push origin pr",
            "bash <<'EOF'\ncd OTHER\ngit push origin pr\nEOF",
            // It may not have run, or ran in a shell of its own.
            "true && cd OTHER; git push origin pr",
            "true && cd OTHER || git push origin pr",
            "false || cd OTHER; git push origin pr",
            "true && cd OTHER; (git push origin pr)",
            "true && cd OTHER; echo $(git push origin pr)",
            "if true; then cd OTHER; fi; git push origin pr",
            "if cd OTHER; then git push origin pr; fi",
            "{ cd OTHER; }; git push origin pr",
            "cd OTHER | cat; git push origin pr",
            "cd OTHER & git push origin pr",
            "cd OTHER && true & git push origin pr",
            "cd OTHER && (true) & git push origin pr",
            "case x in x) cd OTHER;; esac; git push origin pr",
            // Behind something that changes how it runs, or by a name that
            // may be cd.
            "X=1 cd OTHER && git push origin pr",
            "builtin cd OTHER && git push origin pr",
            "command cd OTHER && git push origin pr",
            "eval cd OTHER && git push origin pr",
            "eval 'cd OTHER' && git push origin pr",
            "eval cd OTHER \\; git push origin pr",
            "eval 'cd OTHER;' git push origin pr",
            "x='cd OTHER'; eval \"$x\"; git push origin pr",
            "bash <<'EOF'\ntrue && cd OTHER\ngit push origin pr\nEOF",
            "$c OTHER && git push origin pr",
            "${c} OTHER && git push origin pr",
            // A body read before the cd that runs after it.
            "for i in 1 2; do git push origin pr; cd OTHER; done",
            "while true; do git push origin pr; cd OTHER; done",
            "f() { git push origin pr; }; cd OTHER; f",
            "f() (git push origin pr); cd OTHER; f",
            "function f { git push origin pr; }; cd OTHER; f",
            "f() { git push origin pr; }; (cd OTHER && f)",
            "f() { git push origin pr; }; echo $(cd OTHER && f)",
            "f() { git push origin pr; }; eval 'cd OTHER'; f",
            "for i in 1 2; do git push origin pr; eval 'cd OTHER'; done",
        ],
    );
    assert_each(cwd, &cmds, Some(UNREAD));
    // What it can read and refuses says so first.
    assert_each(
        cwd,
        &at(other.path(), &["cd \"$D\" && git push -f origin pr"]),
        Some(UNLEASED),
    );
}

/// A lease's object is held to the format of the repository the push runs
/// in, and a lease beside a mapping to that repository's `push.default`.
#[test]
fn a_lease_is_read_where_the_cd_went() {
    let sha1 = "0123456789abcdef0123456789abcdef01234567";
    let here = clone_on("pr");
    let cwd = here.path();
    let sha256 = tempfile::tempdir().unwrap();
    git(sha256.path(), &["init", "-q", "--object-format=sha256"]);
    let lease = format!("git push --force-with-lease=pr:{sha1} origin pr");
    assert_eq!(refusal(&lease, cwd), None);
    let cmd = format!("cd {} && {lease}", sha256.path().display());
    assert_eq!(refusal(&cmd, cwd), Some(UNLEASED), "{cmd}");
    let cmd = format!("cd {} && {lease}", cwd.display());
    assert_eq!(refusal(&cmd, sha256.path()), None, "{cmd}");
    let nowhere = tempfile::tempdir().unwrap();
    let cmd = format!("cd {} && {lease}", nowhere.path().display());
    assert_eq!(refusal(&cmd, cwd), Some(UNREAD), "{cmd}");

    let upstream = clone_on("pr");
    git(upstream.path(), &["config", "push.default", "upstream"]);
    let both = format!("git push --force-with-lease --force-with-lease=pr:{sha1} origin pr");
    assert_eq!(refusal(&both, cwd), None);
    let cmd = format!("cd {} && {both}", upstream.path().display());
    assert_eq!(refusal(&cmd, cwd), Some(UNLEASED), "{cmd}");
    let cmd = format!("cd {} && {both}", cwd.display());
    assert_eq!(refusal(&cmd, upstream.path()), None, "{cmd}");
}

/// A force-push with no refspec pushes the branch checked out where it
/// runs: on main, that's R2's.
#[test]
fn a_push_with_no_refspec_pushes_the_branch_where_the_cd_went() {
    let main = clone_on("main");
    let feature = clone_on("feature");
    let (m, f) = (main.path(), feature.path());
    let asks = Some(
        "Force-push or delete of 'main' (R2: rewrites history other clones depend on).".to_owned(),
    );
    for cmd in [
        format!("cd {} && git push -f", m.display()),
        format!("cd {} || exit 1; git push --force-with-lease", m.display()),
        format!("(cd {} && git push -f)", m.display()),
    ] {
        assert_eq!(question(&cmd, f), asks, "{cmd}");
    }
    assert_eq!(
        question(&format!("cd {} && git push -f", f.display()), m),
        None
    );
    assert_eq!(
        question(&format!("(cd {}); git push -f", m.display()), f),
        None
    );
}

/// Subshells nest as deep as the line goes, and each starts from its
/// parent's place: a line of them is read to its end, not the guard's.
#[test]
fn subshells_nested_deep_are_read_through() {
    let here = clone_on("pr");
    let deep = 300_000;
    let cmd = format!(
        "{}git push origin +pr{}",
        "(".repeat(deep),
        ")".repeat(deep)
    );
    assert_eq!(refusal(&cmd, here.path()), Some(UNLEASED));
}
