//! Old code with new tests. No uncertain failure becomes negative evidence.
use super::*;
#[path = "classify.rs"]
mod classify;
#[path = "removal.rs"]
mod removal;
use removal::Change;
// Go's and Rust's sibling files (`foo_test.go`, `foo_tests.rs`) use the
// suffix pattern globally; other suffix-named tests count when they live
// under one of the test-directory patterns.
const SPECS: &[&str] = &[
    ":(glob)tests/**",
    ":(glob)**/tests.rs",
    ":(glob)**/*_tests.rs",
    ":(glob)**/tests/**",
    ":(glob)**/test_*.py",
    ":(glob)**/*_test.go",
    ":(glob)**/*.test.ts",
    ":(glob)src/test/**",
];
fn git(args: &[&str]) -> Command {
    cmd("git", args)
}
fn diff(fork: &str, head: &str, added: bool, specs: &[&str]) -> Result<Vec<String>> {
    let mut c = git(&["diff", "--name-only", "--no-renames", "-z"]);
    if added {
        c.arg("--diff-filter=d");
    }
    c.args([fork, head, "--"]);
    c.args(specs);
    nul(&checked(c)?)
}
fn append_output(k: &str, v: &str) -> Result<()> {
    use std::fs::OpenOptions;
    writeln!(
        OpenOptions::new()
            .append(true)
            .create(true)
            .open(var("GITHUB_OUTPUT"))?,
        "{k}={v}"
    )?;
    Ok(())
}
struct Negative {
    work: Work,
    system: String,
    tests: Vec<String>,
    checks: Vec<String>,
    skipped: Vec<String>,
    unrun: Vec<String>,
    /// The PR's head, where a check that timed out on the negative tree has
    /// to pass.
    head: String,
    /// The merge base, whose code the new tests run on.
    fork: String,
    /// The fork's commit time, which every commit made here takes.
    when: String,
    /// What the negative tree applies to the fork's.
    changes: Vec<Change>,
    /// The lock files whose inputs are in the store apart already, so a
    /// host out of reach costs its timeout once a run, not once a check.
    apart: std::sync::Mutex<Vec<String>>,
}
impl Negative {
    fn found(&self, result: &str, tree: Option<&str>) -> Result<bool> {
        let list = |v: &[String]| {
            format!(
                "[{}]",
                v.iter().map(|v| quote(v)).collect::<Vec<_>>().join(",")
            )
        };
        fs::write(
            temp().join("negative.json"),
            format!(
                "{{\"result\":{},\"tree\":{},\"tests\":{},\"checks\":{},\"skipped\":{},\"unrun\":{}}}\n",
                quote(result),
                tree.map(quote).unwrap_or_else(|| "null".into()),
                list(&self.tests),
                list(&self.checks),
                list(&self.skipped),
                list(&self.unrun)
            ),
        )?;
        append_output("result", result)?;
        Ok(result != "pass")
    }
    fn eval(&self, tree: &Path, name: Option<&str>, apart: bool) -> Command {
        let mut c = cmd("nix", &["eval"]);
        if apart {
            c.args([
                "--store",
                &format!("local?root={}/apart", s(&self.work.0)),
                "--option",
                "substituters",
                "",
            ]);
        }
        c.args([
            "--no-update-lock-file",
            "--option",
            "eval-cache",
            "false",
            "--log-format",
            "internal-json",
        ]);
        let apply = if let Some(name) = name {
            c.arg("--raw");
            format!("o: o.checks.{}.{name}.drvPath", self.system)
        } else {
            c.arg("--json");
            format!("o: builtins.attrNames (o.checks.{} or {{ }})", self.system)
        };
        c.args([&format!("{}#.", s(tree)), "--apply", &apply]);
        c
    }
    fn failed_there(
        &self,
        first: &Output,
        tree: &Path,
        base: &Path,
        name: Option<&str>,
    ) -> Result<()> {
        classify::own(exit_code(first.status), &first.stderr, true)?;
        self.inputs_apart(tree)?;
        let again = output(self.eval(tree, name, true))?;
        if again.status.success() {
            return Err(err("it evaluated on a second try, so it can't tell"));
        }
        classify::own(exit_code(again.status), &again.stderr, false)?;
        if !output(self.eval(base, name, true))?.status.success() {
            return Err(err("the base no longer evaluates either, so it can't tell"));
        }
        Ok(())
    }
    /// The tree's inputs, copied into the store apart: each one fetched
    /// into this store on its own, so one whose host is out of reach stays
    /// out without keeping the rest out, then every input's path, from its
    /// lock, that this store holds. A host the evaluation never reaches for
    /// can't decide the PR, and a second try that reaches for an input it
    /// wasn't given fetched on the way, so can't tell.
    fn inputs_apart(&self, tree: &Path) -> Result<()> {
        let lock = fs::read_to_string(tree.join("flake.lock")).unwrap_or_default();
        let mut apart = self.apart.lock().map_err(|_| err("poisoned"))?;
        if apart.contains(&lock) {
            return Ok(());
        }
        let o = output(cmd(
            "nix",
            &["flake", "prefetch-inputs", "--no-update-lock-file", s(tree)],
        ))?;
        if !o.status.success() {
            classify::errors(&o.stderr);
            println!(
                "not every input could be fetched: those that couldn't aren't given to the second try"
            );
        }
        let o = output(cmd(
            "nix",
            &[
                "flake",
                "archive",
                "--no-update-lock-file",
                "--dry-run",
                "--json",
                s(tree),
            ],
        ))?;
        if !o.status.success() {
            classify::errors(&o.stderr);
            return Err(err(
                "its inputs' paths can't be read from its lock, so it can't tell",
            ));
        }
        // The inputs' paths, not the tree's own: the evaluation copies its
        // source itself, as the base's does.
        let paths = lines(&jq(
            &o.stdout,
            r#".inputs | .. | objects | select(has("path")) | .path"#,
        )?);
        let mut c = cmd("nix-store", &["--check-validity", "--print-invalid"]);
        c.args(&paths);
        let absent = lines(&string(c)?);
        for p in &absent {
            println!("{p}, an input, isn't in this store, so the second try isn't given it");
        }
        let held: Vec<_> = paths.iter().filter(|p| !absent.contains(p)).collect();
        if held.is_empty() {
            apart.push(lock);
            return Ok(());
        }
        let mut c = cmd(
            "nix",
            &[
                "copy",
                "--to",
                &format!("local?root={}/apart", s(&self.work.0)),
            ],
        );
        c.args(held);
        let o = output(c)?;
        if !o.status.success() {
            classify::errors(&o.stderr);
            return Err(err(
                "its inputs can't be copied into a store apart, so it can't tell",
            ));
        }
        apart.push(lock);
        Ok(())
    }
    fn evaluate(&self, tree: &Path, q: &str, root_name: &str) -> Result<Output> {
        let o = output(self.eval(tree, Some(q), false))?;
        if o.status.success() {
            let drv = String::from_utf8_lossy(&o.stdout);
            root(drv.trim_end(), &self.work.0.join("roots").join(root_name))?;
        }
        Ok(o)
    }
}
/// How long a check the negative builds may take once what it's built from
/// has built: Nix's `timeout`, well under the hour of silence every build
/// gets (`SILENT_SECONDS`). A check's build takes minutes: the longest in
/// seven negative runs on 2026-11-19 was burrow-test's, 18½ minutes on a
/// hosted runner with its compile (run 44600817795), so this is that with
/// room for a slower runner. A test that waits forever on the base, as one
/// about a wait ending does on the code with that bug, held run 44510799194
/// for nearly three hours, an hour a build (#1212).
const CHECK_SECONDS: u32 = 30 * 60;
/// How a check built on its own ended.
enum Alone {
    Passed,
    /// Its own build failed, with this log.
    Failed(Vec<u8>),
    /// What it's built from didn't build, or can't be told.
    Unsure,
}
impl Negative {
    /// A check built on its own: what it's built from first, under the
    /// gate's limits (a cold dependency can be slow), once more if that
    /// fails, as a flaky one can; then the check alone, in a child given
    /// [`CHECK_SECONDS`], its log kept to say why it failed.
    fn alone(&self, drv: &str, roots: &Path) -> Result<Alone> {
        let Ok(deps) = classify::inputs(drv) else {
            println!("can't tell what {drv} is built from");
            return Ok(Alone::Unsure);
        };
        if !build::checks(None, &deps, roots)? && !build::checks(None, &deps, roots)? {
            println!("what {drv} is built from didn't build, so it can't tell");
            return Ok(Alone::Unsure);
        }
        // The exact shared builder, heavy's wrapper included, in a child.
        let mut c = merged(
            s(&env::current_exe()?),
            &["--build", s(roots), &format!("{drv}^*")],
        );
        c.env(
            "NIX_CONFIG",
            format!("{}\ntimeout = {CHECK_SECONDS}", var("NIX_CONFIG")),
        );
        let mut log = Vec::new();
        Ok(if tee(c, &mut log, &mut io::stdout())?.code == 0 {
            Alone::Passed
        } else {
            Alone::Failed(log)
        })
    }
    /// Whether a check that failed on its own failed on the new tests:
    /// itself, and not for a file the tree took away (`removal.rs`).
    /// Anything else can't tell.
    fn caught(&self, t: &Target, log: &[u8]) -> Result<bool> {
        let Some(timed_out) = failed_itself(&t.drv, log) else {
            return Ok(false);
        };
        if self.removal_breaks(Some(t))? {
            return self.fails_restored(Some(t), log);
        }
        self.tests_own(t, timed_out)
    }
    /// Whether a check that failed itself is the new tests catching the
    /// base: the same check on the base passes, and, where it timed out, on
    /// the head.
    fn tests_own(&self, t: &Target, timed_out: bool) -> Result<bool> {
        let work = &self.work.0;
        if !t.base.is_empty() && !matches!(self.alone(&t.base, &work.join("roots"))?, Alone::Passed)
        {
            println!(
                "{}, the same check on the base, fails too, so it can't tell",
                t.base
            );
            return Ok(false);
        }
        if timed_out {
            return self.passes_on_head(t, work);
        }
        Ok(true)
    }
    /// A check that timed out with the new tests on the old code, built on
    /// the PR's head: passing there, its timing out is the new tests
    /// catching the base (a test of a wait's end waits forever on code
    /// that never ends it), and isn't built a second time.
    fn passes_on_head(&self, t: &Target, work: &Path) -> Result<bool> {
        println!(
            "{} timed out after {} minutes: building the same check on the head",
            t.drv,
            CHECK_SECONDS / 60
        );
        let tree = work.join("head");
        if !tree.exists() {
            checkout(&self.head, &tree)?;
        }
        let o = self.evaluate(&tree, &t.q, &format!("{}.head", t.i))?;
        let drv = value(&o);
        if drv.is_empty() {
            println!(
                "check {} doesn't evaluate on the head, so its timing out can't tell:",
                t.name
            );
            classify::errors(&o.stderr);
            return Ok(false);
        }
        if drv == t.drv {
            println!("{drv} is the same check on the head, which timed out, so it can't tell");
            return Ok(false);
        }
        if !matches!(self.alone(&drv, &work.join("roots"))?, Alone::Passed) {
            println!(
                "{drv}, the same check on the head, doesn't pass, so its timing out can't tell"
            );
            return Ok(false);
        }
        println!(
            "{} timed out with the new tests on the old code, and passes on the head: the new tests catch the base",
            t.drv
        );
        Ok(true)
    }
}
/// How a check that failed did so itself: whether it timed out, or else
/// its own builder failed with a test's exit code. None is neither, or a
/// fetch, so it can't tell.
fn failed_itself(drv: &str, log: &[u8]) -> Option<bool> {
    if !not_a_fetch(drv) {
        return None;
    }
    let timed_out = classify::timed_out(drv, log);
    if !timed_out && !classify::builder_failed(drv, log) {
        println!(
            "nix doesn't say {drv}'s own builder failed with a test's exit code, or that it timed out, so it can't tell"
        );
        return None;
    }
    Some(timed_out)
}
/// Whether a check is known not to be a fetch, whose failure, or timing
/// out, can be the network's.
fn not_a_fetch(drv: &str) -> bool {
    match classify::fetch(drv) {
        Ok(false) => true,
        Ok(true) => {
            println!("{drv} is a fetch, whose failure can be the network's, so it can't tell");
            false
        }
        Err(_) => {
            println!("can't tell whether {drv} is a fetch, so it can't tell");
            false
        }
    }
}
fn negative_commit(tree: &str, fork: &str, head: &str, when: &str, but: &str) -> Result<String> {
    let mut c = git(&[
        "-c",
        "user.name=flake-check",
        "-c",
        "user.email=flake-check@localhost",
        "-c",
        "commit.gpgsign=false",
        "commit-tree",
        tree,
        "-p",
        fork,
        "-m",
        &format!("the tests of {head} on {fork}'s code{but}"),
    ]);
    c.env("GIT_AUTHOR_DATE", format!("@{when} +0000"))
        .env("GIT_COMMITTER_DATE", format!("@{when} +0000"));
    string(c)
}
fn checkout(rev: &str, to: &Path) -> Result<()> {
    checked(git(&[
        "clone",
        "-q",
        "--shared",
        "--no-checkout",
        ".",
        s(to),
    ]))?;
    checked(git(&["-C", s(to), "checkout", "-q", "--detach", rev]))?;
    Ok(())
}
fn index_git(index: &Path, args: &[&str]) -> Command {
    let mut c = git(args);
    c.env("GIT_INDEX_FILE", index);
    c
}
fn value(o: &Output) -> String {
    if o.status.success() {
        String::from_utf8_lossy(&o.stdout)
            .trim_end_matches('\n')
            .to_owned()
    } else {
        String::new()
    }
}
#[derive(Clone)]
struct Target {
    i: usize,
    name: String,
    q: String,
    drv: String,
    base: String,
}
pub fn negative() -> Result<bool> {
    let mut n = Negative {
        work: Work::new("negative")?,
        system: String::new(),
        tests: vec![],
        checks: vec![],
        skipped: vec![],
        unrun: vec![],
        head: var("HEAD_SHA"),
        fork: String::new(),
        when: String::new(),
        changes: vec![],
        apart: Default::default(),
    };
    let work = n.work.0.clone();
    let base = var("BASE");
    let head = n.head.clone();
    let skip = var("SKIP");
    let tip = string(git(&[
        "rev-parse",
        "--verify",
        &format!("refs/remotes/origin/{base}^{{commit}}"),
    ]))?;
    let fork = string(git(&["merge-base", &tip, &head])).map_err(|_| {
        err(format!(
            "{head} shares no history with {base}, so it has no base code to fail on"
        ))
    })?;
    append_output("fork", &fork)?;
    let changed = diff(&fork, &head, false, &[])?;
    n.tests = diff(&fork, &head, false, SPECS)?;
    let added = diff(&fork, &head, true, SPECS)?;
    if added.is_empty() || changed.iter().all(|f| n.tests.contains(f)) {
        if added.is_empty() {
            println!("{head} adds or changes no test file since {fork}: no negative needed");
        } else {
            println!(
                "{head} changes only test files since {fork}, so the base's code is its own: no negative needed"
            );
        }
        let result = n.found("none", None)?;
        append_output("needs", "false")?;
        return Ok(result);
    }
    println!(
        "the tests {head} adds or changes since {fork}:\n{}",
        added.join("\n")
    );
    append_output("needs", "true")?;
    if var("PATHS_ONLY") == "true" {
        return Ok(true);
    }
    let mut c = git(&[
        "diff",
        "--raw",
        "--no-abbrev",
        "--no-renames",
        "-z",
        &fork,
        &head,
        "--",
    ]);
    c.args(SPECS);
    let raw = nul(&checked(c)?)?;
    n.changes = raw.chunks(2).map(Change::read).collect::<Result<_>>()?;
    let tree = removal::tree_with(&work.join("index"), &fork, &n.changes)?;
    n.when = string(git(&["show", "-s", "--format=%ct", &fork]))?;
    n.fork = fork.clone();
    let commit = negative_commit(&tree, &fork, &head, &n.when, "")?;
    println!("the negative tree: {tree} ({commit})");
    checkout(&fork, &work.join("base"))?;
    checkout(&commit, &work.join("negative"))?;
    n.system = system()?;
    let bn = output(n.eval(&work.join("base"), None, false))?;
    if !bn.status.success() {
        classify::errors(&bn.stderr);
        return Err(err(
            "the base's checks don't evaluate, so there's nothing to compare with",
        ));
    }
    let negative_dir = work.join("negative");
    let base_dir = work.join("base");
    let nn = output(n.eval(&negative_dir, None, false))?;
    if !nn.status.success() {
        classify::errors(&nn.stderr);
        n.failed_there(&nn, &negative_dir, &base_dir, None)?;
        if n.removal_breaks(None)? && !n.fails_restored(None, &[])? {
            return Ok(false);
        }
        println!("the checks don't evaluate with the new tests on the old code: they fail there");
        return n.found("fail", Some(&tree));
    }
    let base_names = lines(&jq(&bn.stdout, ".[]")?);
    let mut names = base_names.clone();
    names.extend(lines(&jq(&nn.stdout, ".[]")?));
    names.sort();
    names.dedup();
    fs::create_dir(work.join("roots"))?;
    let side = memory::side();
    let tasks: Vec<_> = names
        .iter()
        .enumerate()
        .flat_map(|(i, name)| [(i, name, "base"), (i, name, "negative")])
        .collect();
    let evals = memory::parallel(&tasks, side, |(i, name, at)| {
        n.evaluate(&work.join(at), &nix_string(name), &format!("{i}.{at}"))
    });
    let mut targets = Vec::new();
    let mut caught = false;
    let mut unsure = false;
    for (i, name) in names.iter().enumerate() {
        let b = evals[i * 2].as_ref().map_err(|e| err(e.to_string()))?;
        let neg = evals[i * 2 + 1].as_ref().map_err(|e| err(e.to_string()))?;
        let bv = value(b);
        let nv = value(neg);
        let q = nix_string(name);
        if bv.is_empty() && base_names.contains(name) {
            println!(
                "check {name} doesn't evaluate on the base, so there's nothing to compare with:"
            );
            classify::errors(&b.stderr);
            return Ok(false);
        }
        if nv.is_empty() {
            println!("check {name} doesn't evaluate with the new tests on the old code:");
            classify::errors(&neg.stderr);
            n.failed_there(neg, &negative_dir, &base_dir, Some(&q))?;
            n.checks.push(name.clone());
            let t = Target {
                i,
                name: name.clone(),
                q,
                drv: nv,
                base: bv,
            };
            if caught || !n.removal_breaks(Some(&t))? || n.fails_restored(Some(&t), &[])? {
                caught = true;
            } else {
                unsure = true;
            }
        } else if nv == bv {
        } else if skip.split(' ').any(|s| s == name) {
            println!("check {name} reads the new tests, but isn't built here (build-skip)");
            n.skipped.push(name.clone());
        } else {
            println!("check {name} reads the new tests: building it");
            n.checks.push(name.clone());
            targets.push(Target {
                i,
                name: name.clone(),
                q,
                drv: nv,
                base: bv,
            });
        }
    }
    let roots = work.join("roots");
    if !targets.is_empty() {
        println!(
            "each check builds on its own, once what it's built from has: for {} minutes at most",
            CHECK_SECONDS / 60
        );
    }
    let built = memory::parallel(&targets, side, |t| n.alone(&t.drv, &roots));
    for (t, b) in targets.iter().zip(built) {
        match b.map_err(|e| err(e.to_string()))? {
            Alone::Passed => println!("{} built: it passes", t.drv),
            Alone::Unsure => unsure = true,
            Alone::Failed(log) => {
                // One catch is the result, and each costs more builds.
                if caught || n.caught(t, &log)? {
                    caught = true;
                } else {
                    unsure = true;
                }
            }
        }
    }
    if !caught && unsure {
        return Ok(false);
    }
    if caught {
        println!("the new tests fail on the old code, as they should");
        return n.found("fail", Some(&tree));
    }
    let when = n.when.clone();
    coverage(&mut n, &targets, &added, (&fork, &head, &tree, &when), side)?;
    if !n.unrun.is_empty() || !n.skipped.is_empty() {
        if !n.unrun.is_empty() {
            println!(
                "::warning title=New tests the flake doesn't run::no check of the flake runs {} so the negative can't judge them and counts for nothing. If no gate runs them either, they test nothing; give a check that runs them its testFiles.",
                n.unrun.join(" ")
            );
            println!(
                "no check that passed here runs these new tests, so where they'd fail is for the gate that runs them to say:\n{}",
                n.unrun.join("\n")
            );
        }
        if !n.skipped.is_empty() {
            println!(
                "every check built here passed with the new tests on the old code, and those that weren't built (build-skip) are where they'd fail, if anywhere: the caller's runner that builds them has to say"
            );
        }
        n.found("skipped", Some(&tree))
    } else {
        println!(
            "::error title=The new tests pass on the old code::with {head}'s test files on {fork}'s code every check that runs them passes, so they don't test what it changes. Make a test fail without the change, or land a tidy of the tests on its own."
        );
        n.found("pass", Some(&tree))
    }
}
fn coverage(
    n: &mut Negative,
    targets: &[Target],
    added: &[String],
    revision: (&str, &str, &str, &str),
    side: usize,
) -> Result<()> {
    let (fork, head, tree, when) = revision;
    let work = n.work.0.clone();
    let qs = targets
        .iter()
        .map(|t| t.q.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let apply = format!(
        "o: map (n: (o.checks.{}.${{n}} or {{ }}).testFiles or null) [ {qs} ]",
        n.system
    );
    let o = output(cmd(
        "nix",
        &[
            "eval",
            "--no-update-lock-file",
            "--option",
            "eval-cache",
            "false",
            "--log-format",
            "internal-json",
            "--json",
            &format!("{}#.", s(&work.join("base"))),
            "--apply",
            &apply,
        ],
    ))?;
    if !o.status.success() {
        classify::errors(&o.stderr);
        return Err(err(
            "which tests the checks run doesn't evaluate on the base, so it can't tell",
        ));
    }
    let mut covered = Vec::new();
    let mut undeclared = Vec::new();
    for (k, t) in targets.iter().enumerate() {
        // Preserve the JSON type: the string "null" is invalid metadata,
        // not an absent declaration. jq's raw output alone loses that distinction.
        let runs = jq(&o.stdout, &format!(".[{k}] | tojson"))?;
        if runs == "null" {
            undeclared.push(k);
            continue;
        }
        if !jq(
            runs.as_bytes(),
            r#"type == "array" and all(.[]; type == "string" and length > 0 and (test("^[:/]") | not) and (test("[[:cntrl:]]") | not) and (split("/") | index("..") | not))"#,
        ).is_ok_and(|valid| valid == "true")
        {
            return Err(err(format!(
                "{}'s testFiles isn't a list of paths from the repo's root, so it can't tell",
                t.drv
            )));
        }
        let globs: Vec<_> = lines(&jq(runs.as_bytes(), ".[]")?)
            .iter()
            .map(|v| format!(":(glob){v}"))
            .collect();
        let refs: Vec<_> = globs.iter().map(String::as_str).collect();
        if !refs.is_empty() {
            covered.extend(diff(fork, head, true, &refs)?);
        }
    }
    if !undeclared.is_empty() {
        fs::create_dir(work.join("one"))?;
        for (j, f) in added.iter().enumerate() {
            let index = work.join(format!("one/{j}.index"));
            checked(index_git(&index, &["read-tree", tree]))?;
            let at = string(git(&[
                "--literal-pathspecs",
                "ls-tree",
                "--format=%(objectmode),%(objectname)",
                fork,
                "--",
                f,
            ]))?;
            if at.is_empty() {
                checked(index_git(
                    &index,
                    &["update-index", "--force-remove", "--", f],
                ))?;
            } else {
                checked(index_git(
                    &index,
                    &["update-index", "--cacheinfo", &format!("{at},{f}")],
                ))?;
            }
            let one = string(index_git(&index, &["write-tree"]))?;
            let one = negative_commit(&one, fork, head, when, ", but one")?;
            let dir = work.join(format!("one/{j}"));
            fs::create_dir(&dir)?;
            checkout(&one, &dir.join("negative"))?;
            std::os::unix::fs::symlink(work.join("negative"), dir.join("base"))?;
        }
        let tasks: Vec<_> = (0..added.len())
            .flat_map(|j| undeclared.iter().map(move |k| (j, *k)))
            .collect();
        let evals = memory::parallel(&tasks, side, |(j, k)| {
            n.evaluate(
                &work.join(format!("one/{j}/negative")),
                &targets[*k].q,
                &format!("one-{j}-{k}"),
            )
        });
        for j in 0..added.len() {
            for (at, k) in undeclared.iter().enumerate() {
                let o=evals[j*undeclared.len()+at].as_ref().map_err(|e|err(format!("check {}'s evaluation without {} left nothing to read, so it can't tell: {e}",targets[*k].name,added[j])))?;
                if !o.status.success() {
                    println!(
                        "check {} doesn't evaluate without {}:",
                        targets[*k].name, added[j]
                    );
                    classify::errors(&o.stderr);
                    let dir = work.join(format!("one/{j}"));
                    n.failed_there(
                        o,
                        &dir.join("negative"),
                        &dir.join("base"),
                        Some(&targets[*k].q),
                    )?;
                    covered.push(added[j].clone());
                    break;
                }
                if value(o) != targets[*k].drv {
                    covered.push(added[j].clone());
                    break;
                }
            }
        }
    }
    n.unrun = added
        .iter()
        .filter(|f| !covered.contains(f))
        .cloned()
        .collect();
    n.unrun.sort();
    n.unrun.dedup();
    Ok(())
}
