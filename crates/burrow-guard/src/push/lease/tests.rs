use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

use super::{CONFIGURED, UNLEASED, UNREAD, unleased};

/// A new repository, made by `git init ARGS`.
fn repo(args: &[&str]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &[&["init", "-q"], args].concat());
    dir
}

fn git(at: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(at)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// What the guard refuses `cmd` for, run in `at`.
fn refusal(cmd: &str, at: &Path) -> Option<&'static str> {
    let said = unleased(cmd, at, Path::new("git"))?;
    [UNLEASED, CONFIGURED, UNREAD]
        .into_iter()
        .find(|r| *r == said)
}

fn assert_each(at: &Path, cmds: &[String], said: Option<&str>) {
    for cmd in cmds {
        assert_eq!(refusal(cmd, at), said, "{cmd}");
    }
}

fn owned(cmds: &[&str]) -> Vec<String> {
    cmds.iter().map(|c| (*c).to_owned()).collect()
}

/// git's forcing options as git reads them: the last of each wins, an
/// abbreviation is the one option it begins, and `-f`, `--force`, `+`
/// and `--mirror` override any lease.
#[test]
fn a_force_push_needs_a_lease_on_what_its_pusher_saw() {
    let dir = repo(&[]);
    let at = dir.path();
    let oid = "0123456789abcdef0123456789abcdef01234567";
    assert_each(
        at,
        &[
            "git push -f origin pr".to_owned(),
            "git push --force origin pr".to_owned(),
            "git push -uf origin pr".to_owned(),
            "git push origin +pr".to_owned(),
            "git push origin +HEAD:refs/heads/pr".to_owned(),
            "git push --force-with-lease --force-if-includes origin -- +pr".to_owned(),
            "git push --mirror".to_owned(),
            "git push --force-with-lease origin pr".to_owned(),
            "git push --force-with-l origin pr".to_owned(),
            "git push --force-with-lease=pr origin pr".to_owned(),
            "git push --force-with-lease=pr:origin/pr origin pr".to_owned(),
            "git push --force-with-lease=pr:$(git rev-parse origin/pr) origin pr".to_owned(),
            format!("git push --force-with-lease=pr:{} origin pr", &oid[..12]),
            "git push --force-with-lease --force-if-includes -f origin pr".to_owned(),
            "git push --force-with-lease --force-if-includes --no-force-if-includes origin pr"
                .to_owned(),
            "git push --no-force -f --force-with-lease --force-if-includes origin pr".to_owned(),
            format!("git push --force-with-lease=a:{oid} --force-with-lease=b origin a b"),
            "git push origin x && git push -f origin pr".to_owned(),
            "bash -c 'git push -f origin pr'".to_owned(),
            "git -C . push --force origin pr".to_owned(),
        ],
        Some(UNLEASED),
    );
    assert_each(
        at,
        &[
            "git push origin pr".to_owned(),
            "git push -u origin HEAD".to_owned(),
            "git push --force-with-lease --force-if-includes origin pr".to_owned(),
            "git push --force-if-includes --force-with-lease origin HEAD:pr".to_owned(),
            "git push --force-with-l --force-if-i origin pr".to_owned(),
            "git push --force-with-lease=pr --force-if-includes origin pr".to_owned(),
            format!("git push --force-with-lease=pr:{oid} origin pr"),
            "git push --force-with-lease=pr: origin pr".to_owned(),
            "git push -f --no-force --force-with-lease --force-if-includes origin pr".to_owned(),
            "git push --force-with-lease --no-force-with-lease origin pr".to_owned(),
            "git push --force-if-includes origin pr".to_owned(),
            "git push -d origin pr".to_owned(),
            "git push origin :pr".to_owned(),
            "git push -o ci.skip origin pr".to_owned(),
            "echo git push -o -f".to_owned(),
        ],
        None,
    );
}

/// A delete rebuilds nothing onto a stale branch, however it's spelled
/// and whatever forces it; an update beside it is still held to a lease.
#[test]
fn a_push_that_only_deletes_needs_no_lease() {
    let dir = repo(&[]);
    let at = dir.path();
    assert_each(
        at,
        &owned(&[
            "git push -f --delete origin pr",
            "git push --force --delete origin pr",
            "git push -fd origin pr",
            "git push -df origin pr a b",
            "git push --del -f origin pr",
            "git push origin +:pr",
            "git push -f origin :pr :other",
            "git push --force-with-lease --delete origin pr",
            "git push --force-with-lease=pr --delete origin pr",
            "git push --mirror --delete origin pr",
            // What pushes more than its refspecs, undone.
            "git push -f origin :old --tags --no-tags",
            "git push -f --all --no-branches origin :old",
            "git push --mirror --no-mirror origin :old",
        ]),
        None,
    );
    assert_each(
        at,
        &owned(&[
            "git push -d --no-delete -f origin pr",
            "git push --delete --no-del -f origin pr",
            "git push -f origin :old new",
            "git push origin +:old +new",
            "git push origin :old +new",
            "git push --force-with-lease origin :old new",
            // Every tag, or every branch, pushed beside the delete.
            "git push -f origin :old --tags",
            "git push --force origin --ta :old",
            "git push --force-with-lease origin :old --tags",
            "git push -f --all origin :old",
            "git push -f --branches origin :old",
            "git push --mirror origin :old",
            // `+:` is every branch both sides have, forced.
            "git push origin +:",
            "git push -f origin",
        ]),
        Some(UNLEASED),
    );
}

/// `--mirror` sets git's force bit with its own and `--no-mirror` clears
/// both, abbreviated or not; a force set after either still forces.
#[test]
fn mirror_and_its_negation_move_the_force_bit() {
    let dir = repo(&[]);
    let at = dir.path();
    assert_each(
        at,
        &owned(&[
            "git push --mirror --no-mirror origin pr",
            "git push -f --no-mirror origin pr",
            "git push --force --no-mir origin pr",
            "git push --mir --no-mirror origin pr",
            "git push --mirror --no-force origin",
        ]),
        None,
    );
    assert_each(
        at,
        &owned(&[
            "git push --no-mirror -f origin pr",
            "git push --mirror --no-mirror --force origin pr",
            "git push --mir origin",
            "git push --no-force --mirror origin",
        ]),
        Some(UNLEASED),
    );
}

/// A remote configured to force (`remote.NAME.push` with `+`, or
/// `remote.NAME.mirror`) overrides any lease, from the repository's
/// configuration or the command's own, and maps even a branch the push
/// names; a refspec naming its own destination isn't mapped.
#[test]
fn a_remote_configured_to_force_overrides_any_lease() {
    let dir = repo(&[]);
    let at = dir.path();
    let included = at.join("included");
    std::fs::write(
        &included,
        "[remote \"origin\"]\n\tpush = +refs/heads/*:refs/heads/*\n",
    )
    .unwrap();
    assert_each(
        at,
        &[
            "git -c remote.origin.push=+refs/heads/pr:refs/heads/pr push origin".to_owned(),
            "git -c remote.origin.push=+refs/heads/pr:refs/heads/pr push origin pr".to_owned(),
            "git -c remote.origin.push=+refs/heads/pr:refs/heads/pr push --force-with-lease --force-if-includes origin pr".to_owned(),
            "git -c remote.origin.push=+refs/heads/pr:refs/heads/pr push --repo=origin".to_owned(),
            "git -c remote.origin.push=+refs/heads/pr:refs/heads/pr push".to_owned(),
            "git -c remote.origin.mirror=true push origin".to_owned(),
            "git -c remote.origin.mirror push".to_owned(),
            format!("git -c include.path={} push origin pr", included.display()),
        ],
        Some(CONFIGURED),
    );
    assert_each(
        at,
        &owned(&[
            "git -c remote.origin.push=+refs/heads/pr:refs/heads/pr push origin HEAD:refs/heads/pr",
            "git -c remote.origin.push=+refs/heads/pr:refs/heads/pr push origin pr:pr",
            "git -c remote.other.push=+refs/heads/pr:refs/heads/pr push origin pr",
            "git -c remote.origin.push=refs/heads/pr:refs/heads/pr push origin pr",
            "git -c remote.origin.push=+refs/heads/pr:refs/heads/pr push -d origin pr",
            "git -c remote.origin.mirror=false push origin",
            "git -c remote.origin.mirror=true push origin pr",
        ]),
        None,
    );
    // The repository's own configuration.
    git(
        at,
        &[
            "config",
            "remote.origin.push",
            "+refs/heads/pr:refs/heads/pr",
        ],
    );
    assert_each(
        at,
        &owned(&[
            "git push origin pr",
            "git push",
            "git push --force-with-lease=pr:0123456789abcdef0123456789abcdef01234567 origin pr",
        ]),
        Some(CONFIGURED),
    );
    assert_each(
        at,
        &owned(&["git push origin HEAD:refs/heads/pr", "git push x pr"]),
        None,
    );
    let elsewhere = repo(&[]);
    let cmd = format!("git -C {} push origin pr", at.display());
    assert_eq!(refusal(&cmd, elsewhere.path()), Some(CONFIGURED), "{cmd}");
    let cmd = format!("git --git-dir={}/.git push origin pr", at.display());
    assert_eq!(refusal(&cmd, elsewhere.path()), Some(CONFIGURED), "{cmd}");
}

/// What the guard can't read may force anything: an expansion in any of
/// its words or git's, a glob, words from its input, configuration
/// from the environment or `--config-env`. What it can read and refuses
/// says so ahead of what it can't.
#[test]
fn a_push_the_guard_cant_read_is_refused() {
    let dir = repo(&[]);
    let at = dir.path();
    assert_each(
        at,
        &owned(&[
            "git push $(printf -- -f) origin pr",
            "git push `printf -- -f` origin pr",
            // Unquoted, an expansion anywhere splits into words of its own.
            "git push origin pr$(printf ' -f')",
            "git push origin HEAD:refs/heads/$BRANCH",
            "git push -o $(echo 'x -f') origin pr",
            "git push --push-option=$(echo x) origin pr",
            "git -c user.name=$(echo 'a -c remote.origin.mirror') push origin",
            "git -c user.name=$NAME push origin pr",
            "git push \"$FLAGS\" origin pr",
            "git push origin \"$BRANCH\"",
            "git push origin ${BRANCH:-pr}",
            "git push origin -- $(echo +pr)",
            "git push -$X origin pr",
            "git push -u$X origin pr",
            "git push --$X origin pr",
            "git push origin pr*",
            "printf '%s\\n' pr | xargs git push origin",
            "parallel git push origin ::: pr",
            "git $OPTS push origin pr",
            "git -C \"$DIR\" push origin pr",
            "git -c \"remote.origin.push=$SPEC\" push origin pr",
            "git -c \"$SETTING\" push origin pr",
            "git --config-env=remote.origin.push=SPEC push origin pr",
            "git --config-env remote.origin.mirror=M push origin",
            "GIT_CONFIG_PARAMETERS=\"'remote.origin.push'='+pr:pr'\" git push origin pr",
            "GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=remote.origin.mirror GIT_CONFIG_VALUE_0=true git push origin",
            "export GIT_DIR=/elsewhere/.git; git push origin pr",
            "HOME=/elsewhere git push origin pr",
        ]),
        Some(UNREAD),
    );
    assert_each(
        at,
        &owned(&[
            "printf '%s\\n' pr | xargs git push -f origin",
            "printf '%s\\n' pr | xargs git push origin +pr",
            "parallel git push --force origin ::: pr",
            "git push -f origin pr*",
            "git push $(printf -- -f) origin +pr",
            "git push origin \"$BRANCH\" && git push -f origin pr",
        ]),
        Some(UNLEASED),
    );
}

/// git holds each ref it updates to the first lease that names it, and to
/// the bare one only where none does: a bare lease beside named ones that
/// cover every destination checks nothing, so it needs no
/// `--force-if-includes`. Where a destination may be another's, it does.
#[test]
fn a_bare_lease_needs_force_if_includes_only_where_no_named_one_covers() {
    let dir = repo(&[]);
    let at = dir.path();
    let sha = "0123456789abcdef0123456789abcdef01234567";
    let each =
        |cmds: &[&str]| -> Vec<String> { cmds.iter().map(|c| c.replace("SHA", sha)).collect() };
    assert_each(
        at,
        &each(&[
            "git push --force-with-lease --force-with-lease=pr:SHA origin pr",
            "git push --force-with-lease=pr:SHA --force-with-lease origin pr",
            "git push --force-with-lease --force-with-lease=pr:SHA origin HEAD:pr",
            "git push --force-with-lease --force-with-lease=refs/heads/pr:SHA origin x:refs/heads/pr",
            "git push --force-with-lease --force-with-lease=pr:SHA --force-with-lease=b:SHA origin pr x:b",
            "git push --force-with-lease --force-with-lease=pr:SHA origin pr :old",
            "git push --force-with-lease --force-with-lease=pr: origin pr",
        ]),
        None,
    );
    assert_each(
        at,
        &each(&[
            // A destination no named lease covers.
            "git push --force-with-lease --force-with-lease=pr:SHA origin pr other",
            "git push --force-with-lease --force-with-lease=other:SHA origin pr",
            "git push --force-with-lease --force-with-lease=refs/heads/pr:SHA origin pr",
            // Or one the guard can't name: what's checked out, a wildcard,
            // or what git's configuration or --tags, --all, --mirror push.
            "git push --force-with-lease --force-with-lease=pr:SHA origin",
            "git push --force-with-lease --force-with-lease=HEAD:SHA origin HEAD",
            "git push --force-with-lease --force-with-lease=@:SHA origin @",
            "git push --force-with-lease --force-with-lease=pr:SHA origin 'refs/heads/*'",
            "git push --force-with-lease --force-with-lease=pr:SHA origin pr --tags",
            "git push --force-with-lease --force-with-lease=pr:SHA origin pr:",
            // A named lease on the remote-tracking ref still needs it, and
            // git takes the first of two that name one ref.
            "git push --force-with-lease=pr --force-with-lease=pr:SHA origin pr",
            "git push --force-with-lease=pr:origin/pr --force-with-lease=pr:SHA origin pr",
            // The bare lease cleared, the named ones go with it.
            "git push --force-with-lease=pr:SHA --no-force-with-lease --force-with-lease origin pr",
        ]),
        Some(UNLEASED),
    );
}

/// A lease's expected object is a whole object id of the repository's own
/// format, or its compatibility format's: git takes an all-hex name of
/// another length for a ref, which a fetch can move.
#[test]
fn a_lease_names_an_object_in_the_repositorys_format() {
    let sha1 = "0123456789abcdef0123456789abcdef01234567";
    let sha256 = "0123456789abcdef".repeat(4);
    let lease = |oid: &str| format!("git push --force-with-lease=pr:{oid} origin pr");

    let dir = repo(&[]);
    assert_eq!(refusal(&lease(sha1), dir.path()), None);
    assert_eq!(refusal(&lease(&sha256), dir.path()), Some(UNLEASED));

    let dir = repo(&["--object-format=sha256"]);
    assert_eq!(refusal(&lease(&sha256), dir.path()), None);
    assert_eq!(refusal(&lease(sha1), dir.path()), Some(UNLEASED));

    git(dir.path(), &["config", "core.repositoryformatversion", "1"]);
    git(
        dir.path(),
        &["config", "extensions.compatObjectFormat", "sha1"],
    );
    assert_eq!(refusal(&lease(&sha256), dir.path()), None);
    assert_eq!(refusal(&lease(sha1), dir.path()), None);

    // Where git can't say which format, the guard can't either.
    let nowhere = tempfile::tempdir().unwrap();
    assert_eq!(refusal(&lease(sha1), nowhere.path()), Some(UNREAD));
}
