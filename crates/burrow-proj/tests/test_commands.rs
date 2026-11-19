//! Every child a test starts from proj's own programs (`burrow-proj`, its
//! other binaries, or a test binary re-running itself) goes through
//! `burrow_test::command`, which drops the `PROJ_*` environment a proj job
//! runs in. Otherwise a job running these tests gets a different result from
//! CI: the child acts on the job's config and sockets, not the fixture's.
use std::{fs, path::Path};

/// The `Command::new(..)` calls in `source` that start one of proj's own
/// programs, or, in an integration test (`literal_only`), anything but a
/// program named by a string literal.
fn offenders(source: &str, literal_only: bool) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in source.match_indices("Command::new(") {
        let rest = &source[at + "Command::new(".len()..];
        let arg: String = rest
            .chars()
            .take_while(|c| *c != ')')
            .filter(|c| !c.is_whitespace())
            .collect();
        let own = arg.contains("current_exe") || arg.contains("CARGO_BIN_EXE");
        if own || (literal_only && !arg.starts_with('"')) {
            let line = source[..at].matches('\n').count() + 1;
            found.push(format!("line {line}, Command::new starting {arg}"));
        }
    }
    found
}

fn walk(dir: &Path, files: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            walk(&path, files);
        } else if path.extension().is_some_and(|e| e == "rs") {
            files.push(path);
        }
    }
}

#[test]
fn tests_start_proj_programs_without_the_job_environment() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    walk(&root.join("src"), &mut files);
    walk(&root.join("tests"), &mut files);
    let me = Path::new(file!()).file_name().unwrap();
    let mut bad = Vec::new();
    for file in files.iter().filter(|f| f.file_name() != Some(me)) {
        let literal_only = file.starts_with(root.join("tests"));
        for offender in offenders(&fs::read_to_string(file).unwrap(), literal_only) {
            bad.push(format!("{}:{offender}", file.display()));
        }
    }
    assert!(
        bad.is_empty(),
        "start these with burrow_test::command:\n{}",
        bad.join("\n")
    );
}

#[test]
fn the_scan_finds_what_main_had() {
    let host_jobs = "let mut command = Command::new(&cfg.me);";
    let shared = "Command::new(env!(\"CARGO_BIN_EXE_burrow-proj\"))";
    let rerun = "std::process::Command::new(\n    std::env::current_exe().unwrap(),\n)";
    assert_eq!(
        offenders(host_jobs, true),
        ["line 1, Command::new starting &cfg.me"]
    );
    assert!(offenders(host_jobs, false).is_empty());
    assert_eq!(offenders(shared, false).len(), 1);
    assert_eq!(
        offenders(rerun, false),
        ["line 1, Command::new starting std::env::current_exe("]
    );
    assert!(offenders("Command::new(\"git\")", true).is_empty());
    assert!(offenders("burrow_test::command(env::current_exe().unwrap())", false).is_empty());
}
