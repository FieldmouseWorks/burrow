//! Which repos a pass visits, and in which project: each repo once,
//! however many folders are clones of it (a worktree beside its project),
//! so no repo's PRs are listed for the owner twice or handed over again
//! in the same pass.

/// From the projects, in order, each with the repo it's a clone of: the
/// ones a pass visits, `only`'s alone when it's given, each repo once as
/// GitHub matches names (without their case). Its job runs in the folder
/// named for the repo when there's one, else in the first. A hidden folder
/// counts only when it's named for its repo (o/.github in .github).
pub(super) fn visits(clones: Vec<(String, String)>, only: Option<&str>) -> Vec<(String, String)> {
    let named = |project: &str, repo: &str| {
        repo.split_once('/')
            .is_some_and(|(_, own)| own.eq_ignore_ascii_case(project))
    };
    let mut visits: Vec<(String, String)> = Vec::new();
    for (project, repo) in clones {
        if project.starts_with('.') && !named(&project, &repo) {
            continue;
        }
        if only.is_some_and(|only| !only.eq_ignore_ascii_case(&repo)) {
            continue;
        }
        match visits
            .iter_mut()
            .find(|(_, r)| r.eq_ignore_ascii_case(&repo))
        {
            Some(was) if named(&project, &repo) && !named(&was.0, &was.1) => {
                *was = (project, repo);
            }
            Some(_) => {}
            None => visits.push((project, repo)),
        }
    }
    visits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter()
            .map(|(p, r)| (p.to_string(), r.to_string()))
            .collect()
    }

    #[test]
    fn each_repo_is_visited_once_in_the_folder_named_for_it() {
        // remi's /data/dev/src on 2026-11-24: a worktree of burrow beside
        // it, whose ok major was listed for the owner twice.
        let clones = pairs(&[
            ("a-burrow-wt", "FieldmouseWorks/burrow"),
            ("burrow", "FieldmouseWorks/burrow"),
            ("burrow-wt-inbox-owner", "fieldmouseworks/BURROW"),
            ("remi", "FieldmouseWorks/remi"),
            ("remi-copy", "FieldmouseWorks/remi"),
            (".github", "FieldmouseWorks/.github"),
            (".hidden", "FieldmouseWorks/burrow"),
        ]);
        let want = pairs(&[
            ("burrow", "FieldmouseWorks/burrow"),
            ("remi", "FieldmouseWorks/remi"),
            (".github", "FieldmouseWorks/.github"),
        ]);
        assert_eq!(visits(clones.clone(), None), want);
        // One repo's pass: its one visit, however it's spelled.
        assert_eq!(
            visits(clones.clone(), Some("fieldmouseworks/Burrow")),
            want[..1]
        );
        // With no folder named for it, the first.
        let unnamed = pairs(&[("x", "sam/r"), ("y", "Sam/R")]);
        assert_eq!(visits(unnamed, None), pairs(&[("x", "sam/r")]));
    }
}
