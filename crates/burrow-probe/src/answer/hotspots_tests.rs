use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;
use std::process::Command;

use super::{LIMIT, answer};
use crate::Config;
use crate::answer::{Commands, Machine};
use crate::query::{Query, When};

fn git(repo: &Path, args: &[&str], timestamp: Option<i64>) -> String {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    if let Some(ts) = timestamp {
        let val = format!("{ts} +0000");
        cmd.env("GIT_AUTHOR_DATE", &val)
            .env("GIT_COMMITTER_DATE", &val);
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn write_file(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn lines(n: usize) -> String {
    (1..=n).map(|i| format!("let v{i} = {i};\n")).collect()
}

fn probe_config(root: &Path) -> Config {
    serde_json::from_value(serde_json::json!({
        "repo": "sam/host",
        "label": "probe",
        "authors": [],
        "host": "host",
        "user": "dev",
        "stateDir": root,
        "projects": root.join("projects"),
        "proj": null,
        "journal": { "units": [], "userUnits": [] },
        "tools": { "journalctl": "false", "systemctl": "false" }
    }))
    .unwrap()
}

fn init_repo(repo: &Path, remote: &str) {
    fs::create_dir_all(repo).unwrap();
    git(repo, &["init", "-q", "-b", "main"], None);
    git(
        repo,
        &["config", "user.email", "test@example.invalid"],
        None,
    );
    git(repo, &["config", "user.name", "Test"], None);
    git(repo, &["remote", "add", "origin", remote], None);
}

fn set_origin_main(repo: &Path) {
    let head = git(repo, &["rev-parse", "refs/heads/main"], None);
    git(
        repo,
        &["update-ref", "refs/remotes/origin/main", &head],
        None,
    );
    git(
        repo,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
        None,
    );
}

#[test]
fn ranks_by_commits_times_code_lines_and_uses_house_rules() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repo = root.join("projects/tool");
    init_repo(&repo, "https://github.com/sam/tool.git");

    let now = 1_800_000_000i64;
    let day = 86_400i64;

    // 1. Old commit outside the 60-day window: untouched giant file + base files.
    write_file(&repo, "src/untouched_giant.rs", &lines(500));
    write_file(&repo, "src/big_rare.rs", &lines(200));
    write_file(&repo, "src/hot_medium.nix", &lines(80));
    write_file(
        &repo,
        "src/inline_test.rs",
        &format!(
            "{}\n#[cfg(test)]\nmod tests {{\n{}\n}}\n",
            lines(20),
            lines(300),
        ),
    );
    write_file(&repo, "bin/helper", &format!("#!/bin/sh\n{}", lines(49)));
    write_file(&repo, "bin/data", &lines(400));
    write_file(&repo, "tests/huge.rs", &lines(400));
    write_file(&repo, "src/foo_tests.rs", &lines(400));
    write_file(&repo, "src/app.test.ts", &lines(400));
    write_file(&repo, "docs/guide.md", &lines(400));
    write_file(&repo, "src/generated/api.ts", &lines(400));
    write_file(&repo, "src/schema.gen.go", &lines(400));
    write_file(&repo, "src/deleted.rs", &lines(300));
    symlink("src/untouched_giant.rs", repo.join("src/link.rs")).unwrap();
    git(&repo, &["add", "."], None);
    git(&repo, &["commit", "-qm", "old base"], Some(now - 90 * day));

    // 2. Five commits inside the 60-day window touching hot_medium.nix (5 * 80 = 400),
    //    plus test/doc/generated/data files that must be excluded.
    for i in 1..=5 {
        write_file(
            &repo,
            "src/hot_medium.nix",
            &format!("# rev {i}\n{}", lines(79)),
        );
        write_file(&repo, "tests/huge.rs", &format!("// {i}\n{}", lines(400)));
        write_file(
            &repo,
            "src/foo_tests.rs",
            &format!("// {i}\n{}", lines(400)),
        );
        write_file(&repo, "src/app.test.ts", &format!("// {i}\n{}", lines(400)));
        write_file(&repo, "docs/guide.md", &format!("# {i}\n{}", lines(400)));
        write_file(
            &repo,
            "src/generated/api.ts",
            &format!("// {i}\n{}", lines(400)),
        );
        write_file(
            &repo,
            "src/schema.gen.go",
            &format!("// {i}\n{}", lines(400)),
        );
        write_file(&repo, "bin/data", &format!("row {i}\n{}", lines(400)));
        git(&repo, &["add", "."], None);
        git(
            &repo,
            &["commit", "-qm", &format!("churn {i}")],
            Some(now - (40 - i) * day),
        );
    }

    // 3. One commit touching big_rare.rs (1 * 200 = 200) and deleting deleted.rs.
    write_file(
        &repo,
        "src/big_rare.rs",
        &format!("// edit\n{}", lines(199)),
    );
    fs::remove_file(repo.join("src/deleted.rs")).unwrap();
    git(&repo, &["add", "-A"], None);
    git(
        &repo,
        &["commit", "-qm", "touch big and delete"],
        Some(now - 20 * day),
    );

    // 4. Three commits touching bin/helper (shebang script: 3 * 50 = 150)
    //    and two commits touching inline_test.rs (21 prod lines, 300+ cfg(test) lines -> 2 * 21 = 42).
    for i in 1..=3 {
        write_file(
            &repo,
            "bin/helper",
            &format!("#!/bin/sh\n# v{i}\n{}", lines(48)),
        );
        if i <= 2 {
            // 20 lines of prod code + 1 blank line before #[cfg(test)] = 21 prod lines;
            // the 300-line #[cfg(test)] mod must not be counted.
            write_file(
                &repo,
                "src/inline_test.rs",
                &format!(
                    "// v{i}\n{}\n#[cfg(test)]\nmod tests {{\n{}\n}}\n",
                    lines(19),
                    lines(300),
                ),
            );
        }
        git(&repo, &["add", "."], None);
        git(
            &repo,
            &["commit", "-qm", &format!("helper {i}")],
            Some(now - (10 - i) * day),
        );
    }

    // 5. Touch the symlink on main (must be ignored because mode is 120000).
    fs::remove_file(repo.join("src/link.rs")).unwrap();
    symlink("src/big_rare.rs", repo.join("src/link.rs")).unwrap();
    git(&repo, &["add", "src/link.rs"], None);
    git(
        &repo,
        &["commit", "-qm", "retarget symlink"],
        Some(now - 5 * day),
    );

    // Update origin/main and origin/HEAD, then check out a feature branch with
    // extra commits and dirty files to prove hotspots reads origin/main, not HEAD/worktree.
    set_origin_main(&repo);
    git(&repo, &["checkout", "-qb", "feature/wip"], None);
    write_file(&repo, "src/feature_only.rs", &lines(900));
    write_file(&repo, "src/hot_medium.nix", &lines(5));
    git(&repo, &["add", "."], None);
    git(&repo, &["commit", "-qm", "feature commit"], Some(now - day));
    write_file(&repo, "src/hot_medium.nix", "dirty\n");

    let cfg = probe_config(root);
    let m = Machine {
        cfg: &cfg,
        root: root.to_owned(),
        now,
        uid: 1001,
        run: &Commands,
    };

    let q = Query::parse("hotspots sam/tool").unwrap();
    assert_eq!(
        q,
        Query::Hotspots {
            repo: "sam/tool".into(),
            since: When::Ago(60 * 86400),
        }
    );
    let out = m.answer(&q).unwrap();

    // Verify header and ranked rows in exact order.
    let rows: Vec<&str> = out.lines().collect();
    assert!(
        rows[0].starts_with("sam/tool (origin/main @ "),
        "unexpected header: {}",
        rows[0]
    );
    assert!(
        rows[0].contains("(10 commits, 4 code files touched)"),
        "unexpected counts: {}",
        rows[0]
    );
    assert_eq!(
        rows[1].split_whitespace().collect::<Vec<_>>(),
        ["score", "commits", "lines", "path"]
    );

    let parsed: Vec<(u64, usize, usize, &str)> = rows[2..]
        .iter()
        .map(|l| {
            let p: Vec<&str> = l.split_whitespace().collect();
            assert_eq!(p.len(), 4, "bad row: {l}");
            (
                p[0].parse().unwrap(),
                p[1].parse().unwrap(),
                p[2].parse().unwrap(),
                p[3],
            )
        })
        .collect();

    assert_eq!(
        parsed,
        vec![
            (400, 5, 80, "src/hot_medium.nix"),
            (200, 1, 200, "src/big_rare.rs"),
            (150, 3, 50, "bin/helper"),
            (42, 2, 21, "src/inline_test.rs"),
        ],
        "wrong ranking:\n{out}"
    );

    // Explicitly prove excluded files never appear.
    for excluded in [
        "untouched_giant.rs",
        "tests/huge.rs",
        "foo_tests.rs",
        "app.test.ts",
        "docs/guide.md",
        "generated/api.ts",
        "schema.gen.go",
        "bin/data",
        "deleted.rs",
        "link.rs",
        "feature_only.rs",
    ] {
        assert!(!out.contains(excluded), "{excluded} leaked into:\n{out}");
    }

    // Asking a shorter window (-15d) drops the 5 churn commits (at -39..-35d)
    // and the big_rare commit (at -20d), leaving only bin/helper and inline_test.rs.
    let recent = m
        .answer(&Query::parse("hotspots sam/tool -15d").unwrap())
        .unwrap();
    assert!(recent.contains("bin/helper"), "{recent}");
    assert!(recent.contains("src/inline_test.rs"), "{recent}");
    assert!(!recent.contains("src/hot_medium.nix"), "{recent}");
    assert!(!recent.contains("src/big_rare.rs"), "{recent}");
}

#[test]
fn bounds_output_to_top_limit_and_counts_merge_commits_once() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repo = root.join("projects/many");
    init_repo(&repo, "git@github.com:sam/many.git");

    let now = 1_800_000_000i64;
    let day = 86_400i64;

    // Create LIMIT + 5 files on main, each with a distinct line count.
    let total = LIMIT + 5;
    for i in 1..=total {
        write_file(&repo, &format!("src/f{i:02}.rs"), &lines(i * 10));
    }
    git(&repo, &["add", "."], None);
    git(&repo, &["commit", "-qm", "initial"], Some(now - 10 * day));

    // Merge a side branch touching the largest file across two branch commits;
    // on first-parent main, that landed PR counts as 1 commit, not 2.
    git(&repo, &["checkout", "-qb", "pr"], None);
    write_file(&repo, &format!("src/f{total:02}.rs"), &lines(total * 10));
    git(
        &repo,
        &["commit", "--allow-empty", "-qm", "pr step 1"],
        Some(now - 5 * day),
    );
    write_file(
        &repo,
        &format!("src/f{total:02}.rs"),
        &format!("// pr\n{}", lines(total * 10 - 1)),
    );
    git(&repo, &["add", "."], None);
    git(&repo, &["commit", "-qm", "pr step 2"], Some(now - 4 * day));
    git(&repo, &["checkout", "-q", "main"], None);
    git(
        &repo,
        &["merge", "--no-ff", "-qm", "merge pr", "pr"],
        Some(now - 3 * day),
    );
    set_origin_main(&repo);

    let cfg = probe_config(root);
    let m = Machine {
        cfg: &cfg,
        root: root.to_owned(),
        now,
        uid: 1001,
        run: &Commands,
    };
    let out = answer(&m, "sam/many", now - 60 * day).unwrap();
    let lines_out: Vec<&str> = out.lines().collect();
    // Header + column header + LIMIT rows + omission footer.
    assert_eq!(lines_out.len(), 2 + LIMIT + 1, "{out}");
    assert!(
        lines_out[0].contains(&format!(
            "2 commits, {total} code files touched, top {LIMIT}"
        )),
        "{}",
        lines_out[0]
    );
    assert_eq!(
        lines_out.last().copied(),
        Some("(5 more code files omitted)"),
        "{out}"
    );
    // Top file f35.rs was touched in initial + merge = 2 commits, 350 lines -> score 700.
    let top: Vec<&str> = lines_out[2].split_whitespace().collect();
    assert_eq!(
        top,
        vec![
            (2 * total * 10).to_string(),
            "2".into(),
            (total * 10).to_string(),
            format!("src/f{total:02}.rs"),
        ]
    );
}

#[test]
fn prefers_primary_repo_over_worktree_and_reports_unknown_repo() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let primary = root.join("projects/tool");
    init_repo(&primary, "https://github.com/sam/tool.git");

    let now = 1_800_000_000i64;
    write_file(&primary, "src/a.rs", &lines(25));
    git(&primary, &["add", "."], None);
    git(&primary, &["commit", "-qm", "init"], Some(now - 86_400));
    set_origin_main(&primary);

    // Add a linked worktree named earlier alphabetically than `tool` ("a-wt")
    // to prove primary clone is still selected deterministically.
    let wt = root.join("projects/a-wt");
    git(
        &primary,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-branch",
            wt.to_str().unwrap(),
        ],
        None,
    );

    let cfg = probe_config(root);
    let m = Machine {
        cfg: &cfg,
        root: root.to_owned(),
        now,
        uid: 1001,
        run: &Commands,
    };
    let out = answer(&m, "sam/tool", now - 60 * 86_400).unwrap();
    assert!(
        out.contains("25  1  25  src/a.rs") || out.contains("src/a.rs"),
        "{out}"
    );

    let err = answer(&m, "sam/missing", now - 60 * 86_400).unwrap_err();
    assert!(err.contains("no local history for sam/missing"), "{err}");
}

#[test]
fn empty_window_reports_none() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let repo = root.join("projects/quiet");
    init_repo(&repo, "https://github.com/sam/quiet.git");

    let now = 1_800_000_000i64;
    write_file(&repo, "src/old.rs", &lines(100));
    git(&repo, &["add", "."], None);
    git(&repo, &["commit", "-qm", "old"], Some(now - 120 * 86_400));
    set_origin_main(&repo);

    let cfg = probe_config(root);
    let m = Machine {
        cfg: &cfg,
        root: root.to_owned(),
        now,
        uid: 1001,
        run: &Commands,
    };
    let out = answer(&m, "sam/quiet", now - 30 * 86_400).unwrap();
    assert!(out.contains("(0 commits, 0 code files touched):"), "{out}");
    assert!(out.ends_with("\nnone\n"), "{out}");
}
