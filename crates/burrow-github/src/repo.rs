//! Repo names as GitHub allows them.

/// Whether `r` is OWNER/REPO as GitHub allows them, so it can go into a
/// path, a URL or an argument as it is: never one that reads as an option
/// or leaves a directory. A repo's name may start with a dot (`.github`),
/// but isn't `.` or `..`; an owner's is letters, digits and hyphens.
pub fn repo_name(r: &str) -> bool {
    let ok = |s: &str, extra: &str| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || extra.contains(c))
    };
    r.split_once('/')
        .is_some_and(|(o, n)| ok(o, "") && ok(n, "_.") && n != "." && n != "..")
}

#[cfg(test)]
mod tests {
    use super::repo_name;

    #[test]
    fn repo_names_as_github_allows_them() {
        for r in [
            "sam/r",
            "Sam-2/my.repo_x",
            "a/.github",
            "a/_b-c",
            "FieldmouseWorks/.github",
            "o-1/r",
        ] {
            assert!(repo_name(r), "{r}");
        }
        for r in [
            "--help", "--help/x", "sam", "sam/", "/r", "sam/r/x", "sam/-r", "-sam/r", "sa_m/r",
            "sam/r\n", "sam /r", "", "o/.", "o/..", ".o/r", "o/r#1",
        ] {
            assert!(!repo_name(r), "{r:?}");
        }
    }
}
