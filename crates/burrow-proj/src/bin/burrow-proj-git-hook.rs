//! Git hooks installed only in Codex jobs' environment, never repo config.

use std::{
    env,
    io::{self, Read},
    path::Path,
    process::{Command, ExitCode, Stdio},
};

fn git(args: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
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
/// than a guessed name or a potentially stale refs/remotes/origin/HEAD.
fn default_branch(advertisement: &str) -> Result<&str, String> {
    advertisement
        .lines()
        .find_map(|l| l.strip_prefix("ref: ")?.strip_suffix("\tHEAD"))
        .filter(|r| r.starts_with("refs/heads/"))
        .ok_or_else(|| "remote named no default branch; refusing push".into())
}

/// Git passes old and new object ids, not its --force flag. Reject actual
/// non-fast-forward changes and deletions of the advertised default branch.
/// If the old object isn't here, ancestry can't be proved and the push fails.
fn updates(
    input: &str,
    default: &str,
    mut ancestor: impl FnMut(&str, &str) -> bool,
) -> Result<(), String> {
    let zero = |s: &str| !s.is_empty() && s.bytes().all(|b| b == b'0');
    for line in input.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let [_, new, remote, old] = fields.as_slice() else {
            return Err("malformed pre-push input".into());
        };
        if *remote == default && (zero(new) || (!zero(old) && !ancestor(old, new))) {
            return Err(format!("Codex jobs cannot delete or force-push {default}"));
        }
    }
    Ok(())
}

fn run() -> Result<(), String> {
    let args: Vec<_> = env::args().collect();
    match Path::new(&args[0]).file_name().and_then(|s| s.to_str()) {
        Some("commit-msg") => commit_msg(args.get(1).ok_or("commit-msg needs a message file")?),
        Some("pre-push") => {
            let remote = args.get(2).ok_or("pre-push needs the remote URL")?;
            let advertisement = git(&["ls-remote", "--symref", remote, "HEAD"])?;
            let default = default_branch(&advertisement)?;
            let mut input = String::new();
            io::stdin()
                .read_to_string(&mut input)
                .map_err(|e| e.to_string())?;
            updates(&input, default, |old, new| {
                git(&["merge-base", "--is-ancestor", old, new]).is_ok()
            })
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
        let default = default_branch("ref: refs/heads/trunk\tHEAD\naabb\tHEAD\n").unwrap();
        assert!(default_branch("aabb\tHEAD").is_err());
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
}
