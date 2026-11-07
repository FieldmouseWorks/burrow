//! Git hooks installed only in Codex jobs' environment, never repo config.

use std::{
    env,
    io::{self, Read},
    path::Path,
    process::{Command, ExitCode, Stdio},
};

fn git(args: &[&str]) -> Result<String, String> {
    git_in(Path::new("."), args)
}

/// git, run in `dir`.
fn git_in(dir: &Path, args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!("git {} failed", args.join(" ")));
    }
    String::from_utf8(output.stdout).map_err(|e| e.to_string())
}

/// A trailer is added once, including when another Engine trailer exists.
fn commit_msg(file: &str) -> Result<(), String> {
    git(&[
        "interpret-trailers",
        "--in-place",
        "--if-exists",
        "addIfDifferent",
        "--trailer",
        "Engine: codex",
        file,
    ])
    .map(|_| ())
}

/// The remote's advertised HEAD decides which branch is protected, rather
/// than a guessed name or a potentially stale refs/remotes/origin/HEAD. None
/// when it names none: an empty remote (GitHub's, or one that doesn't
/// advertise an unborn HEAD) or one whose HEAD is detached.
fn default_branch(advertisement: &str) -> Option<&str> {
    advertisement
        .lines()
        .find_map(|l| l.strip_prefix("ref: ")?.strip_suffix("\tHEAD"))
        .filter(|r| r.starts_with("refs/heads/"))
}

/// Git passes old and new object ids, not its --force flag. Reject actual
/// non-fast-forward changes and deletions of the advertised default branch.
/// If the old object isn't here, ancestry can't be proved and the push fails.
/// With no default branch named, any branch may be the one, so every branch
/// is protected: what's left is creating refs (an empty remote's first push)
/// and fast-forwarding them.
fn updates(
    input: &str,
    default: Option<&str>,
    mut ancestor: impl FnMut(&str, &str) -> bool,
) -> Result<(), String> {
    let zero = |s: &str| !s.is_empty() && s.bytes().all(|b| b == b'0');
    for line in input.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let [_, new, remote, old] = fields.as_slice() else {
            return Err("malformed pre-push input".into());
        };
        let protected = default.is_none_or(|d| *remote == d);
        if protected && (zero(new) || (!zero(old) && !ancestor(old, new))) {
            return Err(match default {
                Some(d) => format!("Codex jobs cannot delete or force-push {d}"),
                None => format!(
                    "the remote names no default branch, so Codex jobs cannot delete or force-push any of its branches ({remote})"
                ),
            });
        }
    }
    Ok(())
}

/// The pre-push check, in the repository at `dir`, for a push to `remote`
/// (its URL) of `input`, git's lines on the hook's stdin.
fn pre_push(dir: &Path, remote: &str, input: &str) -> Result<(), String> {
    let advertisement = git_in(dir, &["ls-remote", "--symref", remote, "HEAD"])?;
    updates(input, default_branch(&advertisement), |old, new| {
        git_in(dir, &["merge-base", "--is-ancestor", old, new]).is_ok()
    })
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().collect();
    match Path::new(&args[0]).file_name().and_then(|s| s.to_str()) {
        Some("commit-msg") => commit_msg(args.get(1).ok_or("commit-msg needs a message file")?),
        Some("pre-push") => {
            let remote = args.get(2).ok_or("pre-push needs the remote URL")?;
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .map_err(|e| e.to_string())?;
            pre_push(Path::new("."), remote, &input)
        }
        _ => Err("invoke as commit-msg or pre-push".into()),
    }
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("proj git hook: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trailers_are_once_and_only_trailers_count() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("message");
        std::fs::write(&path, "change: do the work\n\nEngine: other\n").unwrap();
        commit_msg(path.to_str().unwrap()).unwrap();
        commit_msg(path.to_str().unwrap()).unwrap();
        let text = std::fs::read_to_string(path).unwrap();
        assert_eq!(text.matches("Engine: codex").count(), 1);
        assert!(text.contains("Engine: other"));
    }
    #[test]
    fn remote_default_not_its_spelling_is_protected() {
        let default = default_branch("ref: refs/heads/trunk\tHEAD\naabb\tHEAD\n");
        assert_eq!(default, Some("refs/heads/trunk"));
        assert_eq!(default_branch("aabb\tHEAD"), None);
        assert_eq!(default_branch(""), None);
        assert!(
            updates(
                "refs/heads/work aabb refs/heads/trunk ccdd",
                default,
                |_, _| true
            )
            .is_ok()
        );
        assert!(
            updates(
                "refs/heads/work aabb refs/heads/trunk ccdd",
                default,
                |_, _| false
            )
            .is_err()
        );
        assert!(updates("(delete) 0000 refs/heads/trunk ccdd", default, |_, _| true).is_err());
        assert!(updates("(delete) 0000 refs/heads/work ccdd", default, |_, _| false).is_ok());
        assert!(
            updates(
                "refs/heads/work aabb refs/heads/trunk 0000",
                default,
                |_, _| false
            )
            .is_ok()
        );
        assert!(updates("bad input", default, |_, _| true).is_err());
    }

    #[test]
    fn with_no_default_branch_every_branch_is_protected_and_creating_is_fine() {
        let create = format!("refs/heads/main aabb refs/heads/main {}", "0".repeat(40));
        assert!(updates(&create, None, |_, _| false).is_ok());
        assert!(
            updates("refs/heads/work aabb refs/heads/work ccdd", None, |_, _| {
                true
            })
            .is_ok()
        );
        assert!(
            updates("refs/heads/work aabb refs/heads/work ccdd", None, |_, _| {
                false
            })
            .is_err()
        );
        let delete = format!("(delete) {} refs/heads/work ccdd", "0".repeat(40));
        assert!(updates(&delete, None, |_, _| true).is_err());
    }

    /// The real thing: a bare remote that advertises no HEAD (as GitHub's
    /// empty repos don't) takes a first push, and refuses a delete after.
    #[test]
    fn an_empty_remote_takes_its_first_push() {
        let dir = tempfile::tempdir().unwrap();
        let remote = dir.path().join("remote.git");
        let local = dir.path().join("local");
        let run = |at: &Path, args: &[&str]| {
            let out = Command::new("git")
                .arg("-C")
                .arg(at)
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {out:?}");
            String::from_utf8(out.stdout).unwrap()
        };
        run(dir.path(), &["init", "-q", "--bare", "remote.git"]);
        run(&remote, &["config", "lsrefs.unborn", "ignore"]);
        run(dir.path(), &["init", "-q", "-b", "main", "local"]);
        run(
            &local,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.test",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
        );
        let url = remote.to_str().unwrap();
        assert_eq!(run(&local, &["ls-remote", "--symref", url, "HEAD"]), "");
        let head = run(&local, &["rev-parse", "HEAD"]);
        let zero = "0".repeat(40);
        let first = format!("refs/heads/main {} refs/heads/main {zero}\n", head.trim());
        pre_push(&local, url, &first).unwrap();
        run(&local, &["push", "-q", url, "main"]);
        let delete = format!("(delete) {zero} refs/heads/main {}\n", head.trim());
        assert!(pre_push(&local, url, &delete).is_err());
    }
}
