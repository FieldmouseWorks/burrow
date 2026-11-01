//! An exclude pattern means what bash's `[[ NAME == PATTERN ]]` says it
//! means, so bash answers: `exclude` was written against it, and its corners
//! (`sam/a*!(|)` matches sam/a, `sam/a*+(|)` doesn't) are what no other
//! matcher would keep.

use std::path::Path;
use std::process::{Command, Stdio};

/// Whether `name` matches `pattern`, as the bash at `bash` answers. Err when
/// it gives no answer: nothing is known about the repo then.
pub fn matches(bash: &Path, pattern: &str, name: &str) -> Result<bool, String> {
    let status = Command::new(bash)
        .args(["--noprofile", "--norc", "-c", "[[ $1 == $2 ]]", "bash"])
        .args([name, pattern])
        .env_remove("BASH_ENV")
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("{}: {e}", bash.display()))?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!("{} on {pattern:?}: {status}", bash.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yes(pattern: &str, name: &str) -> bool {
        matches(Path::new("bash"), pattern, name).unwrap()
    }

    #[test]
    fn answers_as_bash_does() {
        for (pattern, name, want) in [
            ("sam/[^a]*", "sam/b", true),
            ("sam/[^a]*", "sam/a", false),
            ("sam/[[:digit:]]", "sam/7", true),
            ("sam/\\*", "sam/*", true),
            ("sam/\\*", "sam/a", false),
            ("sam/!(a)", "sam/a", false),
            // Collating elements at a range's ends.
            ("sam/[a-[.c.]]", "sam/b", true),
            ("sam/[[.a.]-c]", "sam/b", true),
            ("sam/[[.a.]-[.c.]]", "sam/b", true),
            ("sam/[[.a.]-[.c.]]", "sam/d", false),
            // Groups that can match nothing, with stars and nesting.
            ("sam/a*!(|)", "sam/a", true),
            ("sam/a*+(|)", "sam/a", false),
            ("@(sam/a|sam/*(b|))*?(|)", "sam/b", true),
            // A name or pattern is never read as anything else.
            ("-n", "$(touch nope)", false),
        ] {
            assert_eq!(yes(pattern, name), want, "{name} == {pattern}");
        }
    }

    #[test]
    fn no_bash_is_no_answer() {
        assert!(matches(Path::new("/nonexistent/bash"), "*", "sam/a").is_err());
    }
}
