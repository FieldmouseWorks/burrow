//! Old code with new tests. No uncertain failure becomes negative evidence.
use super::*;
#[path = "classify.rs"]
mod classify;
const SPECS: &[&str] = &[
    ":(glob)tests/**",
    ":(glob)**/tests.rs",
    ":(glob)**/tests/**",
    ":(glob)**/test_*.py",
    ":(glob)**/*_test.*",
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
    fn failed_there(&self, first: &Output, trees: &Path, name: Option<&str>) -> Result<()> {
        classify::own(exit_code(first.status), &first.stderr, true)?;
        let o = output(cmd(
            "nix",
            &[
                "flake",
                "archive",
                "--no-update-lock-file",
                "--to",
                &format!("local?root={}/apart", s(&self.work.0)),
                s(&trees.join("negative")),
            ],
        ))?;
        if !o.status.success() {
            classify::errors(&o.stderr);
            return Err(err(
                "its inputs can't be copied into a store apart, so it can't tell",
            ));
        }
        let again = output(self.eval(&trees.join("negative"), name, true))?;
        if again.status.success() {
            return Err(err("it evaluated on a second try, so it can't tell"));
        }
        classify::own(exit_code(again.status), &again.stderr, false)?;
        if !output(self.eval(&trees.join("base"), name, true))?
            .status
            .success()
        {
            return Err(err("the base no longer evaluates either, so it can't tell"));
        }
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
fn negative_commit(tree: &str, fork: &str, head: &str, when: &str, one: bool) -> Result<String> {
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
        &format!(
            "the tests of {head} on {fork}'s code{}",
            if one { ", but one" } else { "" }
        ),
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
struct Target {
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
    };
    let work = n.work.0.clone();
    let base = var("BASE");
    let head = var("HEAD_SHA");
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
    let index = work.join("index");
    checked(index_git(&index, &["read-tree", &fork]))?;
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
    for pair in raw.chunks(2) {
        if pair.len() != 2 {
            return Err(err("incomplete raw diff"));
        }
        let meta: Vec<_> = pair[0].split_whitespace().collect();
        if meta.len() != 5 {
            return Err(err("bad raw diff"));
        }
        if meta[4] == "D" {
            checked(index_git(
                &index,
                &["update-index", "--force-remove", "--", &pair[1]],
            ))?;
        } else {
            checked(index_git(
                &index,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("{},{},{}", meta[1], meta[3], pair[1]),
                ],
            ))?;
        }
    }
    let tree = string(index_git(&index, &["write-tree"]))?;
    let when = string(git(&["show", "-s", "--format=%ct", &fork]))?;
    let commit = negative_commit(&tree, &fork, &head, &when, false)?;
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
    let nn = output(n.eval(&work.join("negative"), None, false))?;
    if !nn.status.success() {
        classify::errors(&nn.stderr);
        n.failed_there(&nn, &work, None)?;
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
            n.failed_there(neg, &work, Some(&q))?;
            n.checks.push(name.clone());
            caught = true;
        } else if nv == bv {
        } else if skip.split(' ').any(|s| s == name) {
            println!("check {name} reads the new tests, but isn't built here (build-skip)");
            n.skipped.push(name.clone());
        } else {
            println!("check {name} reads the new tests: building it");
            n.checks.push(name.clone());
            targets.push(Target {
                name: name.clone(),
                q,
                drv: nv,
                base: bv,
            });
        }
    }
    let roots = work.join("roots");
    let builds: Vec<_> = targets.iter().map(|t| format!("{}^*", t.drv)).collect();
    if !build::checks(None, &builds, &roots)? {
        let mut unsure = false;
        for t in &targets {
            if classify::built(&t.drv)? {
                continue;
            }
            println!("{} failed: checking that it's its own failure", t.drv);
            let deps = match classify::inputs(&t.drv) {
                Ok(v) => v,
                Err(_) => {
                    println!("can't tell what {} is built from", t.drv);
                    unsure = true;
                    continue;
                }
            };
            if !build::checks(None, &deps, &roots)? {
                println!(
                    "what {} is built from didn't build, so it can't tell",
                    t.drv
                );
                unsure = true;
            } else if !t.base.is_empty()
                && !build::checks(None, &[format!("{}^*", t.base)], &roots)?
            {
                println!(
                    "{}, the same check on the base, fails too, so it can't tell",
                    t.base
                );
                unsure = true;
            } else {
                // Run the exact shared builder in a child to retain its complete
                // diagnostics for classification, including heavy's wrapper.
                let log = work.join("again");
                let o = output(cmd(
                    s(&env::current_exe()?),
                    &["--build", s(&roots), &format!("{}^*", t.drv)],
                ))?;
                show(&o)?;
                let mut text = o.stdout.clone();
                text.extend_from_slice(&o.stderr);
                fs::write(&log, &text)?;
                if o.status.success() {
                    println!("{} built once what it's built from had: it passes", t.drv);
                } else {
                    match classify::fetch(&t.drv) {
                        Ok(false) if classify::builder_failed(&t.drv, &text) => caught = true,
                        Ok(false) => {
                            println!(
                                "nix doesn't say {}'s own builder failed with a test's exit code, so it can't tell",
                                t.drv
                            );
                            unsure = true;
                        }
                        Ok(true) => {
                            println!(
                                "{} is a fetch, whose failure can be the network's, so it can't tell",
                                t.drv
                            );
                            unsure = true;
                        }
                        Err(_) => {
                            println!("can't tell whether {} is a fetch, so it can't tell", t.drv);
                            unsure = true;
                        }
                    }
                }
            }
        }
        if !caught && unsure {
            return Ok(false);
        }
    }
    if caught {
        println!("the new tests fail on the old code, as they should");
        return n.found("fail", Some(&tree));
    }
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
            let one = negative_commit(&one, fork, head, when, true)?;
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
                    n.failed_there(o, &work.join(format!("one/{j}")), Some(&targets[*k].q))?;
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
