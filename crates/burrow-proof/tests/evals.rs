//! Integration tests for `burrow-proof eval-start|eval-run|eval-work|eval-end`.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;

use burrow_proof::end::Ended;
use burrow_proof::eval::{EvalKind, EvalRecord, end, run, start, work};
use serde_json::{Value, json};
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn write_exec(path: &Path, body: &str) {
    burrow_test::executable(path, body);
}

fn which(name: &str) -> PathBuf {
    env::split_paths(&env::var_os("PATH").expect("PATH unset"))
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("{name} not found on PATH"))
}

fn make_mirror(tmp: &Path) -> (PathBuf, String, String, String) {
    let bare = tmp.join("sam__r.git");
    let work = tmp.join("work");
    fs::create_dir_all(&bare).unwrap();
    fs::create_dir_all(&work).unwrap();
    git(&bare, &["init", "-q", "--bare", "-b", "main"]);
    git(&work, &["init", "-q", "-b", "main"]);
    git(&work, &["config", "user.name", "t"]);
    git(&work, &["config", "user.email", "t@t"]);
    git(&work, &["config", "commit.gpgsign", "false"]);
    fs::write(work.join("a"), "1\n").unwrap();
    git(&work, &["add", "a"]);
    git(&work, &["commit", "-qm", "base"]);
    let base = git(&work, &["rev-parse", "HEAD"]);
    // Branch PR 7 from base touching b (clean merge with main).
    git(&work, &["checkout", "-q", "-b", "pr7"]);
    fs::write(work.join("b"), "pr7\n").unwrap();
    git(&work, &["add", "b"]);
    git(&work, &["commit", "-qm", "pr7"]);
    let pr7 = git(&work, &["rev-parse", "HEAD"]);
    // Advance main touching a (non-conflicting with pr7, conflicting with pr8).
    git(&work, &["checkout", "-q", "main"]);
    fs::write(work.join("a"), "main2\n").unwrap();
    git(&work, &["add", "a"]);
    git(&work, &["commit", "-qm", "main2"]);
    let tip = git(&work, &["rev-parse", "HEAD"]);
    // Branch PR 8 from base touching a (conflicts with main2).
    git(&work, &["checkout", "-q", "-b", "pr8", &base]);
    fs::write(work.join("a"), "pr8\n").unwrap();
    git(&work, &["add", "a"]);
    git(&work, &["commit", "-qm", "pr8"]);
    let pr8 = git(&work, &["rev-parse", "HEAD"]);
    git(
        &work,
        &[
            "push",
            "-q",
            bare.to_str().unwrap(),
            &format!("{tip}:refs/base/main"),
            &format!("{pr7}:refs/pr/7"),
            &format!("{pr8}:refs/pr/8"),
        ],
    );
    (bare, tip, pr7, pr8)
}

fn make_fake_nix(tmp: &Path, out_path: &str) -> PathBuf {
    let nix = tmp.join("nix");
    fs::create_dir_all(&nix).unwrap();
    write_exec(
        &nix.join("nix"),
        &format!(
            "#!/bin/sh\necho \"$*\" >> {}/nix.log\nprintf '%s' '{out_path}.drv'\n",
            tmp.display()
        ),
    );
    write_exec(
        &nix.join("nix-store"),
        &format!("#!/bin/sh\nprintf '%s\\n' '{out_path}'\n"),
    );
    nix
}

fn ended(result: &str, code: &str, status: &str) -> Ended {
    Ended {
        result: result.into(),
        code: code.into(),
        status: status.into(),
    }
}

#[test]
fn eval_outputs_start_work_and_end_kicks_repo() {
    let tmp = TempDir::new().unwrap();
    let (mirror, _tip, pr7, _pr8) = make_mirror(tmp.path());
    let out_path = "/nix/store/xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx-proof-x";
    let nix = make_fake_nix(tmp.path(), out_path);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let _ = stream.write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n");
        line
    });

    let dir = tmp.path().join("proof-eval-sam-r-7");
    fs::create_dir_all(&dir).unwrap();
    let mut attrs = BTreeMap::new();
    attrs.insert("agents".into(), "checks.x86_64-linux.agents".into());
    let rec = EvalRecord {
        repo: "sam/r".into(),
        number: 7,
        head: pr7.clone(),
        kind: EvalKind::Outputs,
        mirror: mirror.clone(),
        attrs,
        base: None,
        tip: None,
        unit: "autoland-proof-eval-sam-r-7.service".into(),
        timeout: 900,
        nix,
        git: which("git"),
        heavy: None,
        kick: Some(format!("http://127.0.0.1:{port}/kick/autoland/sam/r")),
        eval: true,
    };
    fs::write(dir.join("build.json"), serde_json::to_vec(&rec).unwrap()).unwrap();

    // Fake systemd-run captures argv.
    let srun = tmp.path().join("systemd-run");
    let captured = tmp.path().join("srun.args");
    write_exec(
        &srun,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
            captured.display()
        ),
    );
    let fake_exe = tmp.path().join("burrow-proof");
    write_exec(&fake_exe, "#!/bin/sh\nexit 0\n");
    start(&dir, &fake_exe, &srun).unwrap();
    let args = fs::read_to_string(&captured).unwrap();
    assert!(
        args.contains("--unit=autoland-proof-eval-sam-r-7.service"),
        "{args}"
    );
    assert!(args.contains("--property=RuntimeMaxSec=900s"), "{args}");
    assert!(args.contains("eval-end"), "{args}");
    assert!(args.contains("eval-run"), "{args}");

    // Run work + end, verify end.json and kick.
    run(&dir, &fake_exe).unwrap();
    let mut said = end(&dir, &ended("success", "exited", "0")).unwrap();
    assert!(said["at"].as_f64().is_some_and(|t| t > 1e9));
    said.as_object_mut().unwrap().remove("at");
    assert_eq!(
        said,
        json!({
            "state": "done",
            "exit": 0,
            "drvs": {"agents": format!("{out_path}.drv")},
            "paths": [out_path],
            "failed": false,
        })
    );
    let req = handle.join().unwrap();
    assert!(req.starts_with("POST /kick/autoland/sam/r "), "{req}");
    let log = fs::read_to_string(tmp.path().join("nix.log")).unwrap();
    assert!(
        log.contains(&format!(
            "?ref=refs/pr/7&rev={pr7}#checks.x86_64-linux.agents.drvPath"
        )),
        "{log}"
    );
}

#[test]
fn eval_merged_handles_clean_merge_conflict_and_timeout() {
    let tmp = TempDir::new().unwrap();
    let (mirror, tip, pr7, pr8) = make_mirror(tmp.path());
    let out_path = "/nix/store/zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz-proof-z";
    let nix = make_fake_nix(tmp.path(), out_path);
    let mut attrs = BTreeMap::new();
    attrs.insert("agents".into(), "checks.x86_64-linux.agents".into());

    // Clean merge (PR 7 onto main): evaluates merged commit.
    let dir7 = tmp.path().join("merged-7");
    fs::create_dir_all(&dir7).unwrap();
    let rec7 = EvalRecord {
        repo: "sam/r".into(),
        number: 7,
        head: pr7,
        kind: EvalKind::Merged,
        mirror: mirror.clone(),
        attrs: attrs.clone(),
        base: Some("main".into()),
        tip: Some(tip.clone()),
        unit: "autoland-proof-eval-7.service".into(),
        timeout: 900,
        nix: nix.clone(),
        git: which("git"),
        heavy: None,
        kick: None,
        eval: true,
    };
    fs::write(dir7.join("build.json"), serde_json::to_vec(&rec7).unwrap()).unwrap();
    work(&dir7).unwrap();
    let said7 = end(&dir7, &ended("success", "exited", "0")).unwrap();
    assert_eq!(said7["state"], "done");
    assert_eq!(said7["paths"], json!([out_path]));
    assert_eq!(said7["failed"], false);

    // Conflicting merge (PR 8 onto main): clean null result, no failure.
    let dir8 = tmp.path().join("merged-8");
    fs::create_dir_all(&dir8).unwrap();
    let rec8 = EvalRecord {
        number: 8,
        head: pr8,
        unit: "autoland-proof-eval-8.service".into(),
        ..rec7.clone()
    };
    fs::write(dir8.join("build.json"), serde_json::to_vec(&rec8).unwrap()).unwrap();
    work(&dir8).unwrap();
    let said8 = end(&dir8, &ended("success", "exited", "0")).unwrap();
    assert_eq!(said8["state"], "done");
    assert_eq!(said8["paths"], Value::Null);
    assert_eq!(said8["failed"], false);

    // Timeout produces timed out + failed: true even if no ran.json exists.
    let dir_to = tmp.path().join("merged-to");
    fs::create_dir_all(&dir_to).unwrap();
    fs::write(
        dir_to.join("build.json"),
        serde_json::to_vec(&rec7).unwrap(),
    )
    .unwrap();
    let said_to = end(&dir_to, &ended("timeout", "killed", "TERM")).unwrap();
    assert_eq!(said_to["state"], "timed out");
    assert_eq!(said_to["failed"], true);
}

#[test]
fn eval_run_wraps_in_heavy_when_configured() {
    let tmp = TempDir::new().unwrap();
    let (mirror, _tip, pr7, _pr8) = make_mirror(tmp.path());
    let nix = make_fake_nix(tmp.path(), "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a");
    let dir = tmp.path().join("eval-heavy");
    fs::create_dir_all(&dir).unwrap();
    let heavy_log = tmp.path().join("heavy.log");
    let heavy = tmp.path().join("heavy");
    write_exec(
        &heavy,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nexit 0\n",
            heavy_log.display()
        ),
    );
    let mut attrs = BTreeMap::new();
    attrs.insert("agents".into(), "checks.x86_64-linux.agents".into());
    let rec = EvalRecord {
        repo: "sam/r".into(),
        number: 7,
        head: pr7,
        kind: EvalKind::Outputs,
        mirror,
        attrs,
        base: None,
        tip: None,
        unit: "autoland-proof-eval-heavy.service".into(),
        timeout: 900,
        nix,
        git: which("git"),
        heavy: Some(heavy),
        kick: None,
        eval: true,
    };
    fs::write(dir.join("build.json"), serde_json::to_vec(&rec).unwrap()).unwrap();
    let fake_exe = tmp.path().join("burrow-proof");
    write_exec(&fake_exe, "#!/bin/sh\nexit 0\n");
    run(&dir, &fake_exe).unwrap();
    let got: Vec<String> = fs::read_to_string(&heavy_log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        got,
        vec![
            "--",
            fake_exe.to_str().unwrap(),
            "eval-work",
            dir.to_str().unwrap(),
        ]
    );
}

#[test]
fn resumed_eval_clears_policy_restoring_and_finishes_done() {
    let tmp = TempDir::new().unwrap();
    let (mirror, _tip, pr7, _pr8) = make_mirror(tmp.path());
    let out_path = "/nix/store/rrrrrrrrrrrrrrrrrrrrrrrrrrrrrrrr-proof-r";
    let nix = make_fake_nix(tmp.path(), out_path);
    let mut attrs = BTreeMap::new();
    attrs.insert("agents".into(), "checks.x86_64-linux.agents".into());
    let dir = tmp.path().join("resumed-eval");
    fs::create_dir_all(&dir).unwrap();
    let rec = EvalRecord {
        repo: "sam/r".into(),
        number: 7,
        head: pr7,
        kind: EvalKind::Outputs,
        mirror,
        attrs,
        base: None,
        tip: None,
        unit: "autoland-proof-eval-resumed.service".into(),
        timeout: 900,
        nix,
        git: which("git"),
        heavy: None,
        kick: None,
        eval: true,
    };
    fs::write(dir.join("build.json"), serde_json::to_vec(&rec).unwrap()).unwrap();
    fs::write(dir.join("policy-restoring"), "paused").unwrap();
    work(&dir).unwrap();
    assert!(!dir.join("policy-restoring").exists());
    let said = end(&dir, &ended("success", "exited", "0")).unwrap();
    assert_eq!(said["state"], "done", "{said}");
    assert_eq!(said["paths"], json!([out_path]));
}

#[test]
fn heavy_queue_wait_does_not_eat_eval_timeout_and_work_enforces_it() {
    let tmp = TempDir::new().unwrap();
    let (mirror, _tip, pr7, _pr8) = make_mirror(tmp.path());
    let out_path = "/nix/store/qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq-proof-q";
    let nix = make_fake_nix(tmp.path(), out_path);
    let mut attrs = BTreeMap::new();
    attrs.insert("agents".into(), "checks.x86_64-linux.agents".into());

    // 1. With heavy configured, systemd-run's RuntimeMaxSec is not set to the
    // short eval timeout (which would kill the unit while waiting in heavy's queue).
    let dir = tmp.path().join("eval-queue");
    fs::create_dir_all(&dir).unwrap();
    let heavy = tmp.path().join("heavy");
    write_exec(
        &heavy,
        "#!/bin/sh\nsleep 1.5\n[ \"$1\" = -- ] && shift\nexec \"$@\"\n",
    );
    let rec = EvalRecord {
        repo: "sam/r".into(),
        number: 7,
        head: pr7,
        kind: EvalKind::Outputs,
        mirror,
        attrs,
        base: None,
        tip: None,
        unit: "autoland-proof-eval-queue.service".into(),
        timeout: 1,
        nix: nix.clone(),
        git: which("git"),
        heavy: Some(heavy),
        kick: None,
        eval: true,
    };
    fs::write(dir.join("build.json"), serde_json::to_vec(&rec).unwrap()).unwrap();

    let srun = tmp.path().join("systemd-run");
    let captured = tmp.path().join("srun.args");
    write_exec(
        &srun,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n",
            captured.display()
        ),
    );
    let fake_exe = tmp.path().join("burrow-proof");
    write_exec(&fake_exe, "#!/bin/sh\nexit 0\n");
    start(&dir, &fake_exe, &srun).unwrap();
    let args = fs::read_to_string(&captured).unwrap();
    assert!(
        !args.contains("--property=RuntimeMaxSec=1s"),
        "heavy queue wait must not be bounded by eval timeout: {args}"
    );

    // 2. When nix itself hangs past rec.timeout inside work(), work() enforces
    // the deadline and end() reports "timed out".
    let slow_nix = tmp.path().join("slow-nix");
    fs::create_dir_all(&slow_nix).unwrap();
    write_exec(&slow_nix.join("nix"), "#!/bin/sh\nsleep 5\n");
    write_exec(&slow_nix.join("nix-store"), "#!/bin/sh\nexit 0\n");
    let dir_to = tmp.path().join("eval-work-timeout");
    fs::create_dir_all(&dir_to).unwrap();
    let rec_to = EvalRecord {
        nix: slow_nix,
        heavy: None,
        unit: "autoland-proof-eval-work-to.service".into(),
        ..rec
    };
    fs::write(
        dir_to.join("build.json"),
        serde_json::to_vec(&rec_to).unwrap(),
    )
    .unwrap();
    assert!(work(&dir_to).is_err());
    let said_to = end(&dir_to, &ended("exit-code", "exited", "1")).unwrap();
    assert_eq!(said_to["state"], "timed out", "{said_to}");
    assert_eq!(said_to["failed"], true, "{said_to}");
}
