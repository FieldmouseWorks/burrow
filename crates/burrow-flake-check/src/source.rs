//! gate-reuse's source: while the gate evaluates and builds, HEAD is a
//! commit of the checkout's tree alone, with no parents and fixed authors
//! and dates, so everything Nix reads of the commit (`self.rev`,
//! `shortRev`, `revCount`, `lastModified`) comes from the tree: two
//! checkouts of one tree get one verdict, whichever commits they were and
//! however much history they fetched. HEAD is put back after, so the steps
//! that follow (the legs' outputs, the attestation) see the checked commit.
use super::*;

/// When every commit of the tree is dated: a constant, so it's the tree's.
const WHEN: &str = "@1 +0000";

pub struct TreeSource {
    top: String,
    reference: String,
    commit: String,
}

fn git(top: &str, args: &[&str]) -> Command {
    let mut c = cmd("git", &["-C", top]);
    c.args(args);
    c
}

impl TreeSource {
    pub fn enter() -> Result<TreeSource> {
        let top = string(cmd("git", &["rev-parse", "--show-toplevel"]))
            .map_err(|e| err(format!("gate-reuse evaluates a git checkout's tree: {e}")))?;
        let reference = output(git(&top, &["symbolic-ref", "-q", "HEAD"]))?;
        let reference = if reference.status.success() {
            text_output(reference.stdout)?
        } else {
            String::new()
        };
        let commit = string(git(&top, &["rev-parse", "--verify", "HEAD^{commit}"]))?;
        let tree = string(git(&top, &["rev-parse", "--verify", "HEAD^{tree}"]))?;
        let mut c = git(
            &top,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit-tree",
                &tree,
                "-m",
                &format!("flake-check: tree {tree}, for gate-reuse"),
            ],
        );
        for (k, v) in [
            ("GIT_AUTHOR_NAME", "flake-check"),
            ("GIT_AUTHOR_EMAIL", "flake-check@localhost"),
            ("GIT_AUTHOR_DATE", WHEN),
            ("GIT_COMMITTER_NAME", "flake-check"),
            ("GIT_COMMITTER_EMAIL", "flake-check@localhost"),
            ("GIT_COMMITTER_DATE", WHEN),
        ] {
            c.env(k, v);
        }
        let source = string(c)?;
        checked(git(&top, &["update-ref", "--no-deref", "HEAD", &source]))?;
        println!(
            "gate-reuse: checking tree {tree} as {source}, a commit of it alone (checked out: {commit})"
        );
        Ok(TreeSource {
            top,
            reference,
            commit,
        })
    }
}

impl Drop for TreeSource {
    fn drop(&mut self) {
        let put = if self.reference.is_empty() {
            checked(git(&self.top, &["update-ref", "--no-deref", "HEAD", &self.commit]))
        } else {
            checked(git(&self.top, &["symbolic-ref", "HEAD", &self.reference]))
        };
        if let Err(e) = put {
            eprintln!("gate-reuse: couldn't put HEAD back at {}: {e}", self.commit);
        }
    }
}
