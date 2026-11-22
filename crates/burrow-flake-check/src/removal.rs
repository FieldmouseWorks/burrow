//! Whether a failure on the negative tree is the tree's own, made by taking
//! away a test file the old code names (#1418). The tree is Conary's to
//! define; whether a failure on it was the test's is decided here.
use super::*;
use std::collections::BTreeMap;
/// A test file's change, from the fork to the head.
#[derive(Clone)]
pub(super) struct Change {
    was_mode: String,
    was_id: String,
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
        let [was_mode, mode, was_id, id, status] = meta[..] else {
            return Err(err("bad raw diff"));
        };
        Ok(Change {
            was_mode: was_mode.trim_start_matches(':').into(),
            was_id: was_id.into(),
            mode: mode.into(),
            id: id.into(),
            status: status.into(),
            path: path.clone(),
        })
    }
    fn deletes(&self) -> bool {
        self.status == "D"
    }
    /// Whether this added file takes a deleted one's place, so that old code
    /// naming that one may find this instead: a directory where the file
    /// was, or the other spelling of a Rust module or target (`x.rs` for
    /// `x/mod.rs` or `x/main.rs`). Whether it does is for a build to say.
    fn replaces(&self, gone: &str) -> bool {
        self.status == "A"
            && (self.within(gone)
                || matches!((module(&self.path), module(gone)), (Some(a), Some(b)) if a == b))
    }
    /// Whether this added file made a deleted one a directory.
    fn within(&self, gone: &str) -> bool {
        self.status == "A" && Path::new(&self.path).starts_with(gone)
    }
    /// Whether no tree holds both this added file and a deleted one: one's
    /// path is the other's directory, either way round.
    fn clashes(&self, gone: &str) -> bool {
        self.within(gone) || (self.status == "A" && Path::new(gone).starts_with(&self.path))
    }
    /// A deleted file's old content, where the file that took its place is.
    fn standing_in(&self, gone: &Change) -> Change {
        Change {
            mode: gone.was_mode.clone(),
            id: gone.was_id.clone(),
            ..self.clone()
        }
    }
}
/// Which of the deleted files a tree to judge by leaves away.
#[derive(Clone, Copy)]
enum Away {
    /// All, as the negative tree does.
    Deleted,
    /// Those the PR replaced, and those no tree holds beside what it added.
    Replaced,
    /// Those no tree holds beside what the PR added: a file that became a
    /// directory, or one in a directory that became a file. The fewest a
    /// tree can.
    Directories,
}
impl Away {
    fn name(self) -> &'static str {
        match self {
            Away::Deleted => "removed",
            Away::Replaced => "restored",
            Away::Directories => "beside",
        }
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
    // The removals first: a deleted file can be in the way of a directory
    // or file of its name the PR adds (#1426).
    let (gone, added): (Vec<&Change>, Vec<&Change>) =
        changes.into_iter().partition(|c| c.deletes());
    for c in gone {
        checked(index_git(
            index,
            &["update-index", "--force-remove", "--", &c.path],
        ))?;
    }
    for c in added {
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
    string(index_git(index, &["write-tree"]))
}
impl Negative {
    /// Whether taking the deleted test files away breaks this check (`None`:
    /// the checks' names) by itself: on the fork's tree with them gone, and
    /// their old content where the PR replaced them. Not being seen to pass
    /// there is breaking it.
    pub(super) fn removal_breaks(&self, t: Option<&Target>) -> Result<bool> {
        if !self.changes.iter().any(Change::deletes) {
            return Ok(false);
        }
        match t {
            Some(t) if t.base.is_empty() => self.reads_deleted(t),
            _ => Ok(!self.stands(t, Away::Deleted)?),
        }
    }
    /// Whether a check the base hasn't reads a deleted test file. No control
    /// passes it (only the new tests define it), so the removal breaks it
    /// unless it gives the same derivation with every deleted file back
    /// where it was, an added file or directory no tree holds beside one
    /// gone to make room: then it reads neither, and its build can't tell
    /// them apart.
    fn reads_deleted(&self, t: &Target) -> Result<bool> {
        let made_room = |c: &Change| {
            self.changes
                .iter()
                .any(|gone| gone.deletes() && c.clashes(&gone.path))
        };
        let applied: Vec<_> = self
            .changes
            .iter()
            .filter(|c| !c.deletes() && !made_room(c))
            .cloned()
            .collect();
        let tree = self.judging_tree("put-back", "every deleted put back", &applied)?;
        let o = self.evaluate(&tree, &t.q, &format!("{}.put-back", t.i))?;
        let reads = t.drv.is_empty() || value(&o) != t.drv;
        println!(
            "check {} is new, so the base has no control for it: {}",
            t.name,
            if reads {
                "it isn't the same with the deleted test files put back, so it reads them"
            } else {
                "it's the same with the deleted test files put back, so their removal can't break it"
            }
        );
        Ok(reads)
    }
    /// The deleted files a tree leaves away.
    fn away(&self, away: Away) -> Vec<Change> {
        let left = |gone: &Change| match away {
            Away::Deleted => true,
            Away::Replaced => self
                .changes
                .iter()
                .any(|a| a.replaces(&gone.path) || a.clashes(&gone.path)),
            Away::Directories => self.changes.iter().any(|a| a.clashes(&gone.path)),
        };
        self.changes
            .iter()
            .filter(|c| c.deletes() && left(c))
            .cloned()
            .collect()
    }
    /// Whether this check passes with the test files where such a tree has
    /// them and nothing new in them: its control. Only where it does is a
    /// failure with the new tests there more than a file's absence, or two
    /// copies' clash.
    fn stands(&self, t: Option<&Target>, away: Away) -> Result<bool> {
        let mut changes = self.away(away);
        for gone in self.changes.iter().filter(|c| c.deletes()) {
            let placed = self.changes.iter().filter(|a| a.replaces(&gone.path));
            changes.extend(placed.map(|a| a.standing_in(gone)));
        }
        if changes.is_empty() {
            // The base, which a failure has to pass on to be the tests'.
            return Ok(true);
        }
        let name = format!("{}-control", away.name());
        let tree = self.judging_tree(&name, "none of the new in them", &changes)?;
        let Some(t) = t else {
            return Ok(output(self.eval(&tree, None, false))?.status.success());
        };
        let o = self.evaluate(&tree, &t.q, &format!("{}.{name}", t.i))?;
        let drv = value(&o);
        if drv.is_empty() || drv == t.drv {
            return Ok(false);
        }
        if drv == t.base {
            return Ok(true);
        }
        let known = self.controls()?.get(&drv).copied();
        if let Some(passed) = known {
            return Ok(passed);
        }
        if !self.builds_here(t) {
            return Ok(false);
        }
        println!(
            "check {} on the base with only its test files' paths changed ({}): building it",
            t.name,
            away.name()
        );
        let passed = matches!(self.alone(&drv, &self.work.0.join("roots"))?, Alone::Passed);
        self.controls()?.insert(drv, passed);
        Ok(passed)
    }
    fn controls(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<String, bool>>> {
        self.controls.lock().map_err(|_| err("poisoned"))
    }
    fn builds_here(&self, t: &Target) -> bool {
        let here = !self.skip.split(' ').any(|s| s == t.name);
        if !here {
            println!(
                "check {} isn't built here (build-skip), so it can't tell",
                t.name
            );
        }
        here
    }
    /// The tree to judge a check the removal breaks by: the new tests, with
    /// the deleted files put back. A replaced one stays away where the old
    /// code finds its replacement, and else goes back where the two can
    /// stand together. None where neither does: it can't tell.
    fn restored(&self, t: Option<&Target>) -> Result<Option<PathBuf>> {
        let what = what(t);
        println!(
            "taking away the test files the PR deletes breaks {what} by itself: trying it with them put back"
        );
        let deleted = self.away(Away::Deleted).len();
        for away in [Away::Replaced, Away::Directories] {
            let left = self.away(away);
            // All of them away is the removal, which breaks it.
            if left.len() == deleted || !self.stands(t, away)? {
                continue;
            }
            let gone = |c: &Change| left.iter().any(|l| l.path == c.path);
            let applied: Vec<_> = self
                .changes
                .iter()
                .filter(|c| !c.deletes() || gone(c))
                .cloned()
                .collect();
            let tree = self.judging_tree(away.name(), "the deleted put back", &applied)?;
            return Ok(Some(tree));
        }
        println!(
            "{what} isn't seen to pass on the base with a test file the PR replaced away, nor with it beside what replaced it: a failure there can be the tree's, not the new tests', so it can't tell. Leave that file where it was."
        );
        Ok(None)
    }
    /// What the checks' names not evaluating on the negative tree says.
    pub(super) fn names_failed(&self) -> Result<Names> {
        if !self.removal_breaks(None)? {
            return Ok(Names::Caught);
        }
        let Some(tree) = self.restored(None)? else {
            return Ok(Names::Unsure);
        };
        let o = output(self.eval(&tree, None, false))?;
        if !o.status.success() {
            classify::errors(&o.stderr);
            self.failed_there(&o, &tree, &self.work.0.join("base"), None)?;
            println!("the checks' names don't evaluate with them put back either");
            return Ok(Names::Caught);
        }
        println!("the checks' names evaluate with them put back: judging each check there");
        Ok(Names::Restored(o.stdout))
    }
    /// Whether a check the removal breaks fails on the tree to judge it by:
    /// only that failure is the tests'. `log` is the negative tree's
    /// build's, which is this one's where it's the same derivation.
    pub(super) fn fails_restored(&self, t: &Target, log: &[u8]) -> Result<bool> {
        let Some(tree) = self.restored(Some(t))? else {
            return Ok(false);
        };
        let what = what(Some(t));
        let o = self.evaluate(&tree, &t.q, &format!("{}.restored", t.i))?;
        if !o.status.success() {
            classify::errors(&o.stderr);
            self.failed_there(&o, &tree, &self.work.0.join("base"), Some(&t.q))?;
            println!("{what} doesn't evaluate with them put back either");
            return Ok(true);
        }
        let drv = value(&o);
        // The base, or the control that passed: no new test is in it.
        if drv == t.base || self.controls()?.get(&drv) == Some(&true) {
            return Ok(tree_made(&what));
        }
        let log = if drv == t.drv {
            log.to_vec()
        } else {
            if !self.builds_here(t) {
                return Ok(false);
            }
            match self.alone(&drv, &self.work.0.join("roots"))? {
                Alone::Passed => return Ok(tree_made(&what)),
                Alone::Unsure => return Ok(false),
                Alone::Failed(log) => log,
            }
        };
        let Some(timed_out) = failed_itself(&drv, &log) else {
            return Ok(false);
        };
        self.tests_own(&Target { drv, ..t.clone() }, timed_out)
    }
    /// A tree to judge a failure by, checked out once. It's never the one
    /// that's signed.
    fn judging_tree(&self, name: &str, what: &str, changes: &[Change]) -> Result<PathBuf> {
        let dir = self.work.0.join(name);
        if !dir.exists() {
            let index = self.work.0.join(format!("{name}.index"));
            let tree = tree_with(&index, &self.fork, changes)?;
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
/// What the checks' names not evaluating on the negative tree says.
pub(super) enum Names {
    /// The new tests fail there.
    Caught,
    /// Nothing can be told.
    Unsure,
    /// The tree broke them, and with the files put back these are the
    /// names, as JSON: each check is judged there, and only a catch counts.
    Restored(Vec<u8>),
}
fn what(t: Option<&Target>) -> String {
    t.map_or("the checks' names".to_owned(), |t| {
        format!("check {}", t.name)
    })
}
fn tree_made(what: &str) -> bool {
    println!(
        "{what} fails with test files the old code names taken away, and not with them put back: the tree made that failure, not the new tests, so it can't tell. A new test has to fail where the old code runs it: one in a file that moved isn't run there, so leave that file where it was, or fail one in a file that stayed."
    );
    false
}
