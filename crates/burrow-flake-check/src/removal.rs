//! Whether a failure on the negative tree is the tree's own, made by taking
//! away a test file the old code names (#1418). The tree is Conary's to
//! define; whether a failure on it was the test's is decided here.
use super::*;
/// A test file's change, from the fork to the head.
pub(super) struct Change {
    mode: String,
    id: String,
    status: String,
    pub(super) path: String,
}
impl Change {
    /// One pair of `git diff --raw -z`.
    pub(super) fn read(pair: &[String]) -> Result<Change> {
        let [meta, path] = pair else {
            return Err(err("incomplete raw diff"));
        };
        let meta: Vec<_> = meta.split_whitespace().collect();
        let [_, mode, _, id, status] = meta[..] else {
            return Err(err("bad raw diff"));
        };
        Ok(Change {
            mode: mode.into(),
            id: id.into(),
            status: status.into(),
            path: path.clone(),
        })
    }
    fn deletes(&self) -> bool {
        self.status == "D"
    }
    /// Whether this added file takes a deleted one's place, so that the two
    /// together would be a failure of their own: a directory where the file
    /// was, or the other spelling of a Rust module or target (`x.rs` for
    /// `x/mod.rs` or `x/main.rs`).
    fn replaces(&self, gone: &str) -> bool {
        self.status == "A"
            && (Path::new(&self.path).starts_with(gone)
                || matches!((module(&self.path), module(gone)), (Some(a), Some(b)) if a == b))
    }
}
fn module(path: &str) -> Option<&str> {
    let p = path.strip_suffix(".rs")?;
    Some(
        p.strip_suffix("/mod")
            .or_else(|| p.strip_suffix("/main"))
            .unwrap_or(p),
    )
}
/// The fork's tree with those changes applied.
pub(super) fn tree_with<'a>(
    index: &Path,
    fork: &str,
    changes: impl IntoIterator<Item = &'a Change>,
) -> Result<String> {
    checked(index_git(index, &["read-tree", fork]))?;
    for c in changes {
        if c.deletes() {
            checked(index_git(
                index,
                &["update-index", "--force-remove", "--", &c.path],
            ))?;
        } else {
            checked(index_git(
                index,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("{},{},{}", c.mode, c.id, c.path),
                ],
            ))?;
        }
    }
    string(index_git(index, &["write-tree"]))
}
impl Negative {
    /// Whether taking the deleted test files away breaks this check (`None`:
    /// the checks' names) by itself, on the fork's tree with nothing else
    /// changed. Not being seen to pass there is breaking it: what that
    /// costs is asking whether it fails with them back.
    pub(super) fn removal_breaks(&self, t: Option<&Target>) -> Result<bool> {
        let deleted: Vec<_> = self.changes.iter().filter(|c| c.deletes()).collect();
        if deleted.is_empty() {
            return Ok(false);
        }
        let tree = self.judging_tree("removed", "only the deleted tests removed", &deleted)?;
        let Some(t) = t else {
            return Ok(!output(self.eval(&tree, None, false))?.status.success());
        };
        let o = self.evaluate(&tree, &t.q, &format!("{}.removed", t.i))?;
        let drv = value(&o);
        if drv.is_empty() || drv == t.drv {
            return Ok(true);
        }
        if drv == t.base {
            return Ok(false);
        }
        println!(
            "check {} on the base with only the deleted test files taken away: building it",
            t.name
        );
        Ok(!matches!(
            self.alone(&drv, &self.work.0.join("roots"))?,
            Alone::Passed
        ))
    }
    /// Whether a check the removal breaks fails with the deleted files put
    /// back beside the new tests too, all but those the PR replaced: only
    /// that failure is the tests'. `log` is the negative tree's build's,
    /// which is this one's where it's the same derivation.
    pub(super) fn fails_restored(&self, t: Option<&Target>, log: &[u8]) -> Result<bool> {
        let what = t.map_or("the checks' names".to_owned(), |t| {
            format!("check {}", t.name)
        });
        println!(
            "taking away the test files the PR deletes breaks {what} by itself: trying it with them put back"
        );
        let applied: Vec<_> = self
            .changes
            .iter()
            .filter(|c| !c.deletes() || self.changes.iter().any(|a| a.replaces(&c.path)))
            .collect();
        let tree = self.judging_tree("restored", "the deleted put back", &applied)?;
        let q = t.map(|t| t.q.as_str());
        let o = match t {
            Some(t) => self.evaluate(&tree, &t.q, &format!("{}.restored", t.i))?,
            None => output(self.eval(&tree, None, false))?,
        };
        if !o.status.success() {
            classify::errors(&o.stderr);
            self.failed_there(&o, &tree, &self.work.0.join("base"), q)?;
            println!("{what} doesn't evaluate with them put back either");
            return Ok(true);
        }
        if let Some(t) = t {
            let drv = value(&o);
            if drv != t.base {
                let log = if drv == t.drv {
                    log.to_vec()
                } else {
                    match self.alone(&drv, &self.work.0.join("roots"))? {
                        Alone::Passed => return Ok(tree_made(&what)),
                        Alone::Unsure => return Ok(false),
                        Alone::Failed(log) => log,
                    }
                };
                let Some(timed_out) = failed_itself(&drv, &log) else {
                    return Ok(false);
                };
                return self.tests_own(&Target { drv, ..t.clone() }, timed_out);
            }
        }
        Ok(tree_made(&what))
    }
    /// A tree to judge a failure by, checked out once. It's never the one
    /// that's signed.
    fn judging_tree(&self, name: &str, what: &str, changes: &[&Change]) -> Result<PathBuf> {
        let dir = self.work.0.join(name);
        if !dir.exists() {
            let index = self.work.0.join(format!("{name}.index"));
            let tree = tree_with(&index, &self.fork, changes.iter().copied())?;
            let commit = negative_commit(
                &tree,
                &self.fork,
                &self.head,
                &self.when,
                &format!(", {what}"),
            )?;
            checkout(&commit, &dir)?;
        }
        Ok(dir)
    }
}
fn tree_made(what: &str) -> bool {
    println!(
        "{what} fails with test files the old code names taken away, and not with them put back: the tree made that failure, not the new tests, so it can't tell. A new test the old code runs has to fail without the change."
    );
    false
}
