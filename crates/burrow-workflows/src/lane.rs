//! Which gate the shared CI gives a PR: flake-check.yml's lane job, its own
//! scripts run on real repos. With pr-gate off (a train repo's), a PR to the
//! default branch gets none, and every push and a PR to any other branch, a
//! branch whose name differs only in case included, still get one. With
//! docs-paths, a PR whose every path is documentation, renames and deletions
//! counted from where the PR left its base, takes the docs lane: only the
//! checks that read docs, in a job of its own named so autoland knows;
//! anything under .github/, a .nix file or a flake.lock never is, whatever
//! the patterns say, and patterns that would match everything are refused.
//! The lane's check run names the base it chose against, the docs lane's the
//! commit the PR left that base at too, and every caller runs again when a
//! PR's base changes, from a workflow file of its own, so that a title or body
//! edit starts no run of the required check's workflow. With docs-paths or
//! legs, the lane job says where the base was, for the legs to merge the head
//! with.

use std::path::{Path, PathBuf};

use yaml_serde::Value;

use crate::harness::{Scratch, read, repo_file, script, step, steps, workflow, write, yaml};

const FLAKE_CHECK: &str = ".github/workflows/flake-check.yml";
/// Every caller, and what runs it again for a new base: burrow's own, and the
/// template every project copies.
const CALLERS: [(&str, &str); 2] = [
    (
        ".github/workflows/flake.yml",
        ".github/workflows/flake-retarget.yml",
    ),
    (
        "templates/project/.github/workflows/flake.yml",
        "templates/project/.github/workflows/flake-retarget.yml",
    ),
];

/// The names autoland gives the lane's and the docs lane's check runs
/// (modules/autoland/prs.py's LANE and DOCS_LANE).
fn autoland_names() -> (String, String) {
    let prs = read(&repo_file("modules/autoland/prs.py"));
    let line = prs
        .lines()
        .find_map(|l| l.strip_prefix("LANE, DOCS_LANE = \""))
        .expect("autoland names no lane");
    let (lane, docs) = line
        .strip_suffix('"')
        .and_then(|l| l.split_once("\", \""))
        .expect("LANE, DOCS_LANE as two strings");
    (lane.to_owned(), docs.to_owned())
}

/// The name autoland knows the gate's job by, and the pattern of its legs'
/// (modules/autoland/prs.py's GATE and LEG).
fn autoland_gate_names() -> (String, String) {
    let prs = read(&repo_file("modules/autoland/prs.py"));
    let line = prs
        .lines()
        .find_map(|l| l.strip_prefix("GATE, LEG = \""))
        .expect("autoland names no gate or legs");
    let (gate, leg) = line
        .strip_suffix("\")")
        .and_then(|l| l.split_once("\", re.compile(\""))
        .expect("GATE, LEG as a string and a compiled pattern");
    (gate.to_owned(), leg.to_owned())
}

#[test]
fn the_jobs_are_wired_as_autoland_reads_them() {
    let wf = workflow(FLAKE_CHECK);
    let inputs = &wf["on"]["workflow_call"]["inputs"];
    assert_eq!(inputs["pr-gate"]["type"], "boolean");
    assert_eq!(inputs["pr-gate"]["default"], true);
    for name in ["docs-paths", "docs-checks"] {
        assert_eq!(inputs[name]["type"], "string", "{name}");
        assert_eq!(inputs[name]["default"], "", "{name}");
    }

    let lane = &wf["jobs"]["lane"];
    // On every PR, whatever the caller turns on: its check run is how
    // autoland knows which base a run judged.
    assert_eq!(lane["if"], "${{ github.event_name == 'pull_request' }}");
    assert_eq!(
        lane["outputs"],
        yaml(
            r#"
            gate: "${{ steps.base.outputs.lands == 'true' && 'train' || steps.paths.outputs.docs == 'true' && 'docs' || 'full' }}"
            fork: "${{ steps.paths.outputs.fork }}"
            tip: "${{ steps.tip.outputs.tip }}"
            "#
        )
    );
    // Both check runs are named after what autoland looks for.
    let (lane_name, docs_name) = autoland_names();
    assert_eq!(
        lane["name"],
        format!("{lane_name}${{{{ github.event.pull_request.base.ref }}}}").as_str()
    );
    assert_eq!(
        step(lane, "base")["env"],
        yaml(
            r#"
            PR_GATE: "${{ inputs.pr-gate }}"
            BASE: "${{ github.event.pull_request.base.ref }}"
            DEFAULT: "${{ github.event.repository.default_branch }}"
            "#
        )
    );
    let lane_steps = steps(lane);
    let checkout = &lane_steps[1];
    assert!(
        checkout["uses"]
            .as_str()
            .is_some_and(|u| u.starts_with("actions/checkout@")),
        "{checkout:?}"
    );
    assert_eq!(
        checkout["with"],
        yaml(
            r#"
            persist-credentials: false
            ref: "${{ github.event.pull_request.head.sha }}"
            fetch-depth: 0
            filter: "blob:none"
            "#
        )
    );
    let (tip, paths) = (&lane_steps[2], &lane_steps[3]);
    assert_eq!(tip["id"], "tip");
    assert_eq!(tip["shell"], "bash");
    assert_eq!(paths["id"], "paths");
    assert_eq!(paths["shell"], "bash");
    // The base's tip for the docs lane or the legs, which docs only for the
    // docs lane.
    for s in [checkout, tip] {
        assert_eq!(
            s["if"],
            "steps.base.outputs.lands != 'true' && (inputs.docs-paths != '' || inputs.legs > 1)",
            "{s:?}"
        );
    }
    assert_eq!(
        paths["if"],
        "steps.base.outputs.lands != 'true' && inputs.docs-paths != ''"
    );
    assert_eq!(
        tip["env"],
        yaml(r#"BASE: "${{ github.event.pull_request.base.ref }}""#)
    );
    assert_eq!(
        paths["env"],
        yaml(
            r#"
            BASE: "${{ github.event.pull_request.base.ref }}"
            TIP: "${{ steps.tip.outputs.tip }}"
            HEAD_SHA: "${{ github.event.pull_request.head.sha }}"
            DOCS: "${{ inputs.docs-paths }}"
            "#
        )
    );

    // The whole gate and the docs lane go by what the lane says (whether each
    // runs, case by case, is pr_base's). The docs lane is a job of its own,
    // under the name autoland looks for.
    let (check, docs) = (&wf["jobs"]["check"], &wf["jobs"]["docs"]);
    assert_eq!(check["needs"], "lane");
    assert_eq!(docs["needs"], "lane");
    // Named check with one leg, as it always was, and after its leg with
    // more, each a name autoland knows for a share of the gate's work.
    assert_eq!(
        check["name"],
        "${{ inputs.legs > 1 && format('leg {0} of {1}', matrix.leg, inputs.legs) || 'check' }}"
    );
    let (gate, leg) = autoland_gate_names();
    assert_eq!(gate, "check");
    let leg = regex::Regex::new(&format!("^(?:{leg})$")).expect("LEG, a regex");
    for n in 2..=8 {
        for k in 1..=n {
            assert!(
                leg.is_match(&format!("leg {k} of {n}")),
                "{leg}: {k} of {n}"
            );
        }
    }
    for not in ["leg 1 of 1", "leg 0 of 2", "leg 9 of 9", "check"] {
        assert!(!leg.is_match(not), "{leg}: {not}");
    }
    assert_eq!(
        docs["name"],
        format!(
            "{docs_name}${{{{ github.event.pull_request.base.ref }}}} from ${{{{ needs.lane.outputs.fork }}}}"
        )
        .as_str()
    );
    // It reads the tree and attests nothing.
    assert_eq!(docs["permissions"], yaml("contents: read"));
    assert!(
        !steps(docs)
            .iter()
            .any(|s| s["uses"].as_str().is_some_and(|u| u.contains("attest"))),
        "{:?}",
        docs["steps"]
    );
    let docs_steps = steps(docs);
    assert_eq!(
        docs_steps[2]["if"],
        "inputs.install-nix && inputs.docs-checks != ''"
    );
    assert_eq!(
        docs_steps[3]["env"],
        yaml(r#"CHECKS: "${{ inputs.docs-checks }}""#)
    );
}

/// Every caller runs again on a new base, and only on that edit: autoland
/// counts a run only for the base it judged, and which paths a PR changes
/// depend on its base.
/// The jobs of a workflow that call the shared CI.
fn calls(caller: &Value) -> Vec<(&Value, &Value)> {
    caller["jobs"]
        .as_mapping()
        .expect("jobs")
        .iter()
        .filter(|(_, j)| {
            j["uses"]
                .as_str()
                .is_some_and(|u| u.split('@').next().unwrap().ends_with("/flake-check.yml"))
        })
        .collect()
}

/// Autoland counts a run only for the base it judged, and which paths a PR
/// changes depend on its base, so every caller runs again on a new base.
/// GitHub can only filter on the `edited` action, so a title or body edit
/// starts a run of whichever workflow takes it, and there the job skips.
/// flake.yml mustn't be that workflow: its skipped run would be its latest, and
/// a ruleset requiring `check / check` would stop counting the push's
/// (consolebook#173).
#[test]
fn callers_run_again_on_a_new_base_from_a_file_of_their_own() {
    for (path, retarget) in CALLERS {
        let caller = workflow(path);
        let mine = calls(&caller);
        assert!(!mine.is_empty(), "{path} doesn't call flake-check.yml");
        let types = &caller["on"]["pull_request"]["types"];
        assert!(
            !types
                .as_sequence()
                .is_some_and(|t| t.iter().any(|t| t == "edited")),
            "{path} takes `edited`: {:?}",
            caller["on"]
        );
        for (_, j) in &mine {
            assert!(j.get("if").is_none(), "{path} skips: {:?}", j["if"]);
        }

        let twin = workflow(retarget);
        assert_eq!(
            twin["on"],
            yaml("pull_request:\n  types: [edited]\n"),
            "{retarget} runs on anything but `edited`"
        );
        // The one name is what makes a retarget supersede the push's run, and
        // the next push the retarget's: github.workflow is in each concurrency
        // group.
        assert_eq!(twin["name"], caller["name"], "{retarget}");
        assert_eq!(twin["permissions"], caller["permissions"], "{retarget}");
        let its = calls(&twin);
        assert_eq!(its.len(), mine.len(), "{retarget}");
        // Only a new base, and otherwise the same call: the same check.
        for ((name, j), (their_name, theirs)) in mine.iter().zip(&its) {
            assert_eq!(name, their_name, "{retarget}");
            assert_eq!(theirs["if"], "github.event.changes.base", "{retarget}");
            let mut same = (*theirs).clone();
            same.as_mapping_mut().unwrap().remove("if");
            assert_eq!(&same, *j, "{retarget}: {name:?}");
        }
    }
    // Autoland knows the pair by these files (modules/autoland/prs.py's FLAKE).
    let prs = read(&repo_file("modules/autoland/prs.py"));
    let (flake, retarget) = CALLERS[0];
    assert!(
        prs.contains(&format!("FLAKE = (\"{flake}\", \"{retarget}\")")),
        "autoland doesn't know {flake} and {retarget} as a pair"
    );
    let burrow = workflow(CALLERS[0].0);
    assert!(
        burrow["jobs"]
            .as_mapping()
            .unwrap()
            .values()
            .any(|j| j["with"]["docs-paths"]
                .as_str()
                .is_some_and(|d| !d.is_empty())),
        "burrow's caller lists no docs"
    );
}

/// The base step's word on a PR to `base`, in a repo whose default branch is
/// `default`.
fn lands(s: &Scratch, base: &str, default: &str, pr_gate: &str) -> String {
    let wf = workflow(FLAKE_CHECK);
    let ran = s.bash(
        script(step(&wf["jobs"]["lane"], "base")),
        s.path(),
        &[("BASE", base), ("DEFAULT", default), ("PR_GATE", pr_gate)],
    );
    assert_eq!(ran.code, 0, "{}", ran.said());
    ran.outputs
}

/// Only the exact default branch is the train's: a case variant is another
/// branch, and so is any other name. With pr-gate on, no PR is.
#[test]
fn the_train_lands_only_its_default_branchs_prs() {
    let s = Scratch::default();
    assert_eq!(lands(&s, "main", "main", "false"), "lands=true\n");
    for other in ["Main", "MAIN", "release", "main2", ""] {
        assert_eq!(
            lands(&s, other, "main", "false"),
            "lands=false\n",
            "{other}"
        );
    }
    assert_eq!(lands(&s, "main", "main", "true"), "lands=false\n");
}

/// GitHub, with main holding a bit of everything, the docs included, and
/// the PRs made off it.
struct Repo {
    s: Scratch,
    origin: PathBuf,
    work: PathBuf,
    /// Where every PR leaves main.
    fork: String,
    prs: usize,
}

/// What a PR changes.
#[derive(Default)]
struct Change<'a> {
    files: &'a [&'a str],
    gone: &'a [&'a str],
    moved: &'a [(&'a str, &'a str)],
}

/// The docs lane's patterns burrow-like repos give.
const ALL: &str = "README.md AGENTS.md docs/** **/*.md";

impl Repo {
    fn new() -> Repo {
        let s = Scratch::default();
        let (origin, work) = (s.join("origin.git"), s.join("work"));
        s.git(
            s.path(),
            &["init", "-q", "--bare", origin.to_str().unwrap()],
        );
        s.git(s.path(), &["init", "-q", work.to_str().unwrap()]);
        let mut repo = Repo {
            s,
            origin,
            work,
            fork: String::new(),
            prs: 0,
        };
        let work = repo.work.clone();
        repo.fork = repo.change(
            &work,
            &Change {
                files: &[
                    "README.md",
                    "AGENTS.md",
                    "docs/guide.md",
                    "docs/deep/more.md",
                    "src/main.rs",
                    "src/notes.md",
                    "archive/old.md",
                    "flake.nix",
                    "flake.lock",
                    "nix/module.nix",
                    "sub/flake.lock",
                    "Cargo.toml",
                    ".github/workflows/flake.yml",
                    ".github/pull_request_template.md",
                ],
                ..Change::default()
            },
        );
        repo.push(&work, "main");
        repo
    }

    fn git(&self, cwd: &Path, args: &[&str]) -> String {
        self.s.git(cwd, args)
    }

    fn push(&self, cwd: &Path, branch: &str) {
        self.git(cwd, &["push", "-q", self.origin.to_str().unwrap(), branch]);
    }

    /// A commit writing each of `files`, deleting each of `gone` and
    /// renaming each of `moved`.
    fn change(&self, cwd: &Path, c: &Change) -> String {
        for f in c.files {
            let path = cwd.join(f);
            let mut text = std::fs::read_to_string(&path).unwrap_or_default();
            text.push_str(&format!("{f} changed\n"));
            write(&path, &text);
            self.git(cwd, &["add", f]);
        }
        for f in c.gone {
            self.git(cwd, &["rm", "-q", f]);
        }
        for (from, to) in c.moved {
            std::fs::create_dir_all(cwd.join(to).parent().unwrap()).unwrap();
            self.git(cwd, &["mv", from, to]);
        }
        self.git(cwd, &["commit", "-qm", "change", "--allow-empty"]);
        self.git(cwd, &["rev-parse", "HEAD"])
    }

    /// A clone as the lane job's checkout leaves it, at `head`.
    fn checkout(&self, head: &str) -> PathBuf {
        let ws = self.s.mkdtemp();
        let (origin, at) = (self.origin.to_str().unwrap(), ws.to_str().unwrap());
        self.git(self.s.path(), &["clone", "-q", "--no-checkout", origin, at]);
        self.git(&ws, &["checkout", "-q", "--detach", head]);
        ws
    }

    /// The paths step on a PR branch off main that makes the change `c`,
    /// after main itself moved on by changing `main_moves`: what it did, and
    /// the head.
    fn pr(&mut self, docs: &str, c: &Change, main_moves: &[&str]) -> (crate::harness::Ran, String) {
        self.prs += 1;
        let branch = format!("pr{}", self.prs);
        let work = self.work.clone();
        self.git(&work, &["checkout", "-q", "-b", &branch, &self.fork]);
        let head = self.change(&work, c);
        self.push(&work, &branch);
        if !main_moves.is_empty() {
            self.git(&work, &["checkout", "-q", "main"]);
            self.change(
                &work,
                &Change {
                    files: main_moves,
                    ..Change::default()
                },
            );
            self.push(&work, "main");
        }
        let ws = self.checkout(&head);
        let ran = self.paths(&ws, &head, docs);
        (ran, head)
    }

    fn paths(&self, ws: &Path, head: &str, docs: &str) -> crate::harness::Ran {
        let wf = workflow(FLAKE_CHECK);
        let tip = self.tip_of(ws);
        self.s.bash(
            script(step(&wf["jobs"]["lane"], "paths")),
            ws,
            &[
                ("BASE", "main"),
                ("TIP", &tip),
                ("HEAD_SHA", head),
                ("DOCS", docs),
            ],
        )
    }

    /// The tip step, as the lane job's checkout leaves it: where the base is.
    fn tip_of(&self, ws: &Path) -> String {
        let wf = workflow(FLAKE_CHECK);
        let ran = self.s.bash(
            script(step(&wf["jobs"]["lane"], "tip")),
            ws,
            &[("BASE", "main")],
        );
        let want = self.git(&self.origin, &["rev-parse", "main"]);
        assert!(
            ran.code == 0
                && ran.outputs == format!("tip={want}\n")
                && ran.said().contains(&format!("main is at {want}")),
            "{} {} {} (main at {want})",
            ran.code,
            ran.outputs,
            ran.said()
        );
        want
    }

    /// Whether the PR takes the docs lane, and what the step said. It names
    /// the commit the PR left main at, as git has it.
    fn lane_of(&mut self, docs: &str, c: &Change, main_moves: &[&str]) -> (bool, String) {
        let (ran, head) = self.pr(docs, c, main_moves);
        assert_eq!(ran.code, 0, "{}", ran.said());
        let fork_now = self.git(&self.origin, &["merge-base", "main", &head]);
        let docs_lane = ran.outputs == format!("fork={fork_now}\ndocs=true\n");
        assert!(
            docs_lane || ran.outputs == format!("fork={fork_now}\ndocs=false\n"),
            "{} (fork {fork_now}): {}",
            ran.outputs,
            ran.said()
        );
        (docs_lane, ran.said())
    }

    fn docs(&mut self, docs: &str, c: &Change) -> bool {
        self.lane_of(docs, c, &[]).0
    }
}

fn files<'a>(files: &'a [&'a str]) -> Change<'a> {
    Change {
        files,
        ..Change::default()
    }
}

#[test]
fn the_docs_lane_takes_documentation_only() {
    let mut r = Repo::new();
    // Documentation only, as the patterns say: the docs lane. `*` stays in
    // its directory and `**` crosses them, the root included.
    assert!(r.docs(ALL, &files(&["README.md"])));
    assert!(r.docs(ALL, &files(&["docs/guide.md", "docs/deep/more.md"])));
    assert!(r.docs(ALL, &files(&["src/notes.md"]))); // **/*.md
    assert!(r.docs("*.md", &files(&["README.md", "AGENTS.md"])));
    assert!(!r.docs("*.md", &files(&["src/notes.md"])));
    assert!(!r.docs("docs/*", &files(&["docs/deep/more.md"])));
    // Deleted, renamed or new documentation is still documentation.
    let gone = Change {
        gone: &["docs/guide.md"],
        ..Change::default()
    };
    assert!(r.docs(ALL, &gone));
    let renamed = Change {
        moved: &[("docs/guide.md", "docs/renamed.md")],
        ..Change::default()
    };
    assert!(r.docs(ALL, &renamed));
    assert!(r.docs(ALL, &files(&["docs/new.md"])));
    // Anything else in the change: the whole gate, which says what isn't
    // documentation.
    let (docs_lane, said) = r.lane_of(ALL, &files(&["README.md", "src/main.rs"]), &[]);
    let (_, not_docs) = said.split_once("aren't documentation:").expect(&said);
    assert!(
        !docs_lane && not_docs.contains("src/main.rs") && !not_docs.contains("README.md\n"),
        "{said}"
    );
    assert!(!r.docs(ALL, &files(&["Cargo.toml"])));
    // A rename counts where it started: code moved into docs/ is code leaving.
    let code_out = Change {
        moved: &[("src/main.rs", "docs/main.md")],
        ..Change::default()
    };
    assert!(!r.docs(ALL, &code_out));
    // A pattern left out with `!` isn't documentation.
    assert!(!r.docs("**/*.md !archive/**", &files(&["archive/old.md"])));
    assert!(r.docs("**/*.md !archive/**", &files(&["docs/guide.md"])));
    // What defines the gate never is, whatever the patterns say: .github/
    // (its Markdown too), every .nix file, and any flake.lock.
    for f in [
        ".github/workflows/flake.yml",
        ".github/pull_request_template.md",
        "flake.nix",
        "nix/module.nix",
        "flake.lock",
        "sub/flake.lock",
    ] {
        assert!(!r.docs("** .github/**", &files(&[f])), "{f}");
        assert!(!r.docs("** .github/**", &files(&["README.md", f])), "{f}");
    }
    let template_out = Change {
        moved: &[(".github/pull_request_template.md", "docs/template.md")],
        ..Change::default()
    };
    assert!(!r.docs("**", &template_out));
    // Only what the PR changes counts: main moving on with code meanwhile is
    // main's, not the PR's.
    assert!(r.lane_of(ALL, &files(&["README.md"]), &["src/main.rs"]).0);
    // A PR that changes nothing gets the whole gate.
    assert!(!r.docs(ALL, &Change::default()));
}

/// Patterns that would match everything, or that are pathspec magic, are
/// refused: the job fails, saying why, rather than guessing.
#[test]
fn docs_paths_that_would_take_everything_are_refused() {
    let mut r = Repo::new();
    for (bad, why) in [
        (":(top)README.md", "can't start with ':'"),
        (":!src/**", "can't start with ':'"),
        ("!src/** !flake.nix", "names no documentation"),
    ] {
        let (ran, _) = r.pr(bad, &files(&["README.md"]), &[]);
        assert!(
            ran.code != 0 && ran.stdout.contains(why) && !ran.outputs.contains("docs="),
            "{bad}: {} {}",
            ran.code,
            ran.said()
        );
    }
}

/// A head with no history in common with its base gets the whole gate.
#[test]
fn unrelated_histories_get_the_whole_gate() {
    let r = Repo::new();
    let ws = r.checkout(&r.fork);
    r.git(&ws, &["checkout", "-q", "--orphan", "alone"]);
    r.git(&ws, &["rm", "-rqf", "."]);
    let alone = r.change(&ws, &files(&["README.md"]));
    let ran = r.paths(&ws, &alone, ALL);
    assert!(
        ran.code == 0 && ran.outputs == "docs=false\n" && ran.said().contains("shares no history"),
        "{} {} {}",
        ran.code,
        ran.outputs,
        ran.said()
    );
}

/// The docs lane's build: each check docs-checks names, for this system,
/// and nothing when it names none, without asking nix anything.
#[test]
fn the_docs_lane_builds_only_the_docs_checks() {
    let s = Scratch::default();
    let log = s.join("nix-log");
    s.stub(
        "nix",
        &format!(
            "echo \"$*\" >> {}\n[ \"$1 $2\" = \"config show\" ] && echo x86_64-linux\nexit 0\n",
            log.display()
        ),
    );
    let wf = workflow(FLAKE_CHECK);
    let build = script(&steps(&wf["jobs"]["docs"])[3]);
    write(&log, "");
    let ran = s.bash(build, s.path(), &[("CHECKS", "")]);
    assert!(
        ran.code == 0 && ran.said().contains("nothing to build") && read(&log).is_empty(),
        "{}",
        ran.said()
    );
    let ran = s.bash(build, s.path(), &[("CHECKS", "lychee  docs")]);
    assert_eq!(ran.code, 0, "{}", ran.said());
    assert_eq!(
        read(&log).lines().collect::<Vec<_>>(),
        [
            "config show system",
            "build -L --no-link --keep-going .#checks.x86_64-linux.lychee .#checks.x86_64-linux.docs"
        ]
    );
}
