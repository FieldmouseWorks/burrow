use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::comment::{comment, cut};
use crate::start::{argv, start};
use crate::{Ran, Record, summary, verdict};

fn record() -> Record {
    Record {
        repo: "sam/r".into(),
        number: 5,
        head: "a".repeat(40),
        checkout: "/srv/projects/r".into(),
        runs: BTreeMap::from([("gate".to_owned(), "make gate".to_owned())]),
        unit: "autoland-proof-sam-r-5-aaaaaaaaaaaa-1.service".into(),
        timeout: 10800,
        bash: "/bin/bash".into(),
        git: "/bin/git".into(),
        trees: "/data/scratch/proofs".into(),
        kick: Some("http://127.0.0.1:8788/kick/autoland/sam/r".into()),
        comment_bytes: 60000,
    }
}

fn written(dir: &Path, value: serde_json::Value) {
    fs::write(dir.join("build.json"), value.to_string()).unwrap();
}

#[test]
fn a_record_is_held_to_what_a_run_needs() {
    let dir = tempfile::tempdir().unwrap();
    let good = serde_json::to_value(record()).unwrap();
    written(dir.path(), good.clone());
    assert_eq!(Record::read(dir.path()).unwrap(), record());
    for (key, value, why) in [
        ("head", json!("a".repeat(12)), "isn't a commit's full hash"),
        ("repo", json!("sam"), "isn't OWNER/REPO"),
        ("number", json!(0), "number 0"),
        ("checkout", json!("r"), "checkout r isn't an absolute path"),
        (
            "trees",
            json!("proofs"),
            "trees proofs isn't an absolute path",
        ),
        ("runs", json!({}), "no runs"),
        (
            "unit",
            json!("autoland-x y.service"),
            "isn't autoland-ID.service",
        ),
        ("unit", json!("other.service"), "isn't autoland-ID.service"),
        ("timeout", json!(0), "timeout 0"),
        ("base", json!("main"), "unknown field `base`"),
    ] {
        let mut bad = good.clone();
        bad[key] = value;
        written(dir.path(), bad);
        let got = Record::read(dir.path()).unwrap_err();
        assert!(got.contains(why), "{key}: {got}");
    }
}

#[test]
fn an_exit_is_a_verdict_only_from_0_to_125() {
    assert_eq!(verdict(0), Some(true));
    for exit in [1, 2, 100, 125] {
        assert_eq!(verdict(exit), Some(false), "{exit}");
    }
    for exit in [126, 127, 128, 137, 143, 255, -1] {
        assert_eq!(verdict(exit), None, "{exit}");
    }
}

#[test]
fn its_unit_is_bounded_divided_and_ended_by_systemd() {
    let rec = record();
    assert_eq!(
        argv(
            "/nix/store/x/bin/burrow-proof",
            "/var/lib/autoland/builds/ID",
            &rec
        ),
        [
            "--user",
            "--quiet",
            "--collect",
            "--unit=autoland-proof-sam-r-5-aaaaaaaaaaaa-1.service",
            "--property=Delegate=yes",
            "--property=RuntimeMaxSec=10800s",
            "--property=ExecStopPost=:/nix/store/x/bin/burrow-proof end /var/lib/autoland/builds/ID",
            "--expand-environment=no",
            "--",
            "/nix/store/x/bin/burrow-proof",
            "run",
            "/var/lib/autoland/builds/ID",
        ]
    );
}

/// A stand-in systemd-run that keeps its arguments and exits RC.
fn systemd_run(dir: &Path, rc: i32) -> PathBuf {
    let path = dir.join("systemd-run");
    fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >{}/args\nexit {rc}\n",
            dir.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn start_hands_systemd_the_run_or_says_why_not() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("builds/proof-sam-r-5-1");
    fs::create_dir_all(&dir).unwrap();
    written(&dir, serde_json::to_value(record()).unwrap());
    let exe = Path::new("/nix/store/x/bin/burrow-proof");
    start(&dir, exe, &systemd_run(tmp.path(), 0)).unwrap();
    let args = fs::read_to_string(tmp.path().join("args")).unwrap();
    let dir_s = dir.to_str().unwrap();
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        argv(exe.to_str().unwrap(), dir_s, &record())
    );
    // systemd's refusal, as it says it.
    let why = start(&dir, exe, &systemd_run(tmp.path(), 1)).unwrap_err();
    assert!(
        why.starts_with("systemd-run refused autoland-proof-sam-r-5-aaaaaaaaaaaa-1.service"),
        "{why}"
    );
    // A folder its command line would have to quote is never handed over.
    let odd = tmp.path().join("builds/a b");
    fs::create_dir_all(&odd).unwrap();
    written(&odd, serde_json::to_value(record()).unwrap());
    fs::remove_file(tmp.path().join("args")).unwrap();
    let why = start(&odd, exe, &systemd_run(tmp.path(), 0)).unwrap_err();
    assert!(
        why.contains("isn't a path a unit's command line takes as it is"),
        "{why}"
    );
    assert!(!tmp.path().join("args").exists());
    // Nor a record it can't run.
    written(&dir, json!({"repo": "sam/r"}));
    assert!(start(&dir, exe, &systemd_run(tmp.path(), 0)).is_err());
    assert!(!tmp.path().join("args").exists());
}

#[test]
fn a_comment_fits_whatever_its_summaries_hold() {
    let dir = tempfile::tempdir().unwrap();
    let mut rec = record();
    rec.runs = BTreeMap::from([
        ("a".to_owned(), "x".to_owned()),
        ("b".to_owned(), "y".to_owned()),
    ]);
    for i in 0..2 {
        fs::write(summary(dir.path(), i), "界".repeat(40000)).unwrap();
    }
    let ran = Ran {
        verdicts: BTreeMap::from([
            ("a".to_owned(), ["failure".to_owned(), String::new()]),
            ("b".to_owned(), ["success".to_owned(), String::new()]),
        ]),
        exits: BTreeMap::from([("a".to_owned(), 1), ("b".to_owned(), 0)]),
    };
    let said = comment(&rec, dir.path(), "proof-1", &ran).unwrap();
    assert!(said.len() <= rec.comment_bytes, "{}", said.len());
    assert_eq!(said.matches('…').count(), 2);
    assert!(
        said.starts_with(&format!(
            "`a` failed on {}: it ran here and exited 1.\n\n界",
            rec.head
        )),
        "{}",
        &said[..200]
    );
    assert!(said.contains(&format!("`b` passed on {}.", rec.head)));
    // Each cut lands on a character's edge.
    assert_eq!(cut("界界", 5), "\n…");
    assert_eq!(cut("界界界", 8), "界\n…");
    assert_eq!(cut("界界", 6), "界界");
    assert_eq!(cut("界界", 3), "");
}
