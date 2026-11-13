use burrow_github::Failed;
use burrow_github::fake::{Fake, ok};
use serde_json::{Value, json};

use super::*;

const BURROW: &str = r#"["actions", "c-cpp", "javascript-typescript", "python", "rust"]"#;

fn changed(names: &[&str]) -> Value {
    Value::Array(names.iter().map(|n| json!({"filename": n})).collect())
}

/// What a PR, #7 of sam/r, with these files picks of `project`, and what
/// it said.
fn pr(project: &str, files: Value) -> (Vec<&'static str>, String) {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/r/pulls/7/files",
        vec![ok(200, files, &[])],
    );
    let mut log = Vec::new();
    let picked = pick(
        &mut gh,
        "sam/r",
        &Event::PullRequest(7),
        &languages(project).unwrap(),
        &mut log,
    )
    .unwrap();
    (
        picked.iter().map(|l| l.name()).collect(),
        String::from_utf8(log).unwrap(),
    )
}

fn picked(project: &str, files: &[&str]) -> Vec<&'static str> {
    pr(project, changed(files)).0
}

const EVERY: [&str; 5] = [
    "actions",
    "c-cpp",
    "javascript-typescript",
    "python",
    "rust",
];

/// A push (the default branch's) scans every language, without asking GitHub
/// what changed: the Fake has nothing scripted and panics if asked.
#[test]
fn a_push_scans_every_language() {
    let mut log = Vec::new();
    let got = pick(
        &mut Fake::default(),
        "sam/r",
        &Event::Push,
        &languages(BURROW).unwrap(),
        &mut log,
    )
    .unwrap();
    assert_eq!(got.iter().map(|l| l.name()).collect::<Vec<_>>(), EVERY);
}

/// A PR scans only the languages of the files it changes.
#[test]
fn a_pr_scans_the_languages_it_touches() {
    assert_eq!(
        picked(
            BURROW,
            &[
                "crates/burrow-proj/src/inbox/plans.rs",
                "docs/task-planning.md"
            ]
        ),
        ["rust"]
    );
    // A workflow is YAML, and a lock file is YAML to the JavaScript
    // extractor, which reads both.
    assert_eq!(
        picked(
            BURROW,
            &[
                ".github/workflows/flake.yml",
                "modules/autoland/step.py",
                "Cargo.lock"
            ]
        ),
        ["actions", "javascript-typescript", "python", "rust"]
    );
}

/// The tables cover what the extractors read, case aside: the JavaScript
/// extractor's every file type (its HTML templates and SAP's `.xsjs`
/// included), Python's `.pyw`, and each language's configuration wherever
/// it's kept.
#[test]
fn the_tables_cover_what_the_extractors_read() {
    let langs = r#"["c-cpp", "javascript-typescript", "python", "ruby", "rust"]"#;
    for (file, want) in [
        ("src/widget.es6", &["javascript-typescript"][..]),
        ("views/index.ejs", &["javascript-typescript"]),
        ("web/App.TSX", &["javascript-typescript"]),
        ("hana/service.xsjs", &["javascript-typescript"]),
        ("hana/lib.XSJSLIB", &["javascript-typescript"]),
        ("web/page.jsp", &["javascript-typescript"]),
        ("tpl/row.html.dot", &["javascript-typescript"]),
        ("yarn.lock", &["javascript-typescript"]),
        ("views/show.html.erb", &["javascript-typescript", "ruby"]),
        ("lib/thing.gemspec", &["ruby"]),
        ("Gemfile.lock", &["javascript-typescript", "ruby"]),
        ("native/Vec.HPP", &["c-cpp"]),
        ("tools/run.PYW", &["python"]),
        ("requirements/dev.txt", &["c-cpp", "python"]),
        ("CMakeLists.txt", &["c-cpp", "python"]),
        ("tox.ini", &["python"]),
        ("rust-toolchain.toml", &["python", "rust"]),
        (".cargo/config.toml", &["python", "rust"]),
        ("rust-project.json", &["javascript-typescript", "rust"]),
        (
            "build/compile_commands.json",
            &["c-cpp", "javascript-typescript"],
        ),
    ] {
        assert_eq!(picked(langs, &[file]), want, "{file}");
    }
}

/// A path with no extension scans every language, whatever its name: the
/// JavaScript and Python extractors read one by its shebang, and a name
/// can't rule that out. So does an extension the tables don't know.
#[test]
fn what_it_cannot_classify_scans_everything() {
    for file in [
        "bin/deploy",
        "bin/license",
        "bin/notice",
        "LICENSE",
        "Gemfile",
        "Makefile",
        "crates/x/.cargo/config",
        "web/.babelrc",
        "web/.prettierignore",
        "src/thing.xyz",
    ] {
        let (got, said) = pr(BURROW, changed(&[file, "a.rs"]));
        assert_eq!(got, EVERY, "{file}");
        assert!(
            said.contains(&format!("{file}: nothing here says")),
            "{said}"
        );
    }
}

/// A PR that touches no scanned language's files scans nothing, and says
/// why for each.
#[test]
fn a_docs_pr_scans_nothing() {
    let (got, said) = pr(
        BURROW,
        changed(&["README.md", "modules/proj.nix", "docs/flow.svg"]),
    );
    assert!(got.is_empty(), "{got:?}");
    assert!(
        said.contains("rust: the PR changes none of its files"),
        "{said}"
    );
}

/// A rename counts its old path too: moving Python out of a file is a Python
/// change.
#[test]
fn a_rename_counts_its_old_path() {
    let (got, _) = pr(
        BURROW,
        json!([{"filename": "tools/sync.md", "previous_filename": "tools/sync.py"}]),
    );
    assert_eq!(got, ["python"]);
}

/// A PR with as many paths as GitHub will list scans every language.
#[test]
fn a_pr_too_big_to_list_scans_everything() {
    let names: Vec<String> = (0..MOST_FILES).map(|i| format!("docs/{i}.md")).collect();
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    assert_eq!(picked(BURROW, &names), EVERY);
}

/// A language the workflow can't analyze with no build on Linux is an
/// error naming it, before any event is looked at.
#[test]
fn a_language_it_cannot_analyze_fails() {
    let err = languages(r#"["rust", "go"]"#).unwrap_err();
    assert!(
        err.starts_with("go: this workflow doesn't analyze it"),
        "{err}"
    );
    assert!(languages("rust").is_err());
}

/// When GitHub can't say what changed, picking fails rather than scan
/// nothing.
#[test]
fn a_failed_listing_fails() {
    let mut gh = Fake::default();
    gh.on(
        "GET",
        "repos/sam/r/pulls/7/files",
        vec![Err(Failed {
            why: "no answer".into(),
            transient: false,
        })],
    );
    let err = pick(
        &mut gh,
        "sam/r",
        &Event::PullRequest(7),
        &languages(BURROW).unwrap(),
        &mut Vec::new(),
    )
    .unwrap_err();
    assert!(err.contains("listing PR #7's files"), "{err}");
}

/// The matrix is the project's order, as a JSON array.
#[test]
fn the_matrix_is_a_json_array() {
    assert_eq!(
        matrix(&[Language::Rust, Language::Actions]),
        r#"["rust","actions"]"#
    );
    assert_eq!(matrix(&[]), "[]");
}
