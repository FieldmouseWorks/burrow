//! renovate-sweep on a machine made of files, with stand-ins for gh, git,
//! proj, the notifier, major-seal and lock-wait: which Renovate PRs go to a
//! Claude job, what the owner's notices list, which verdicts count, and the
//! lock file refreshes that only wait for their releases to be old enough.
//! What major-seal makes of a token or a seal, and lock-wait of Renovate's
//! comments, is theirs (tests/renovate-sweep.py): here major-seal takes the
//! seals FAKE/seals lists, and lock-wait says the time on the line of
//! FAKE/rest-N (Renovate's comments on PR N) that names who it was asked
//! about.
//!
//! The stand-ins note how they were run in FAKE/log; each job and notice is
//! a file of its own, its words \0-separated. A file in FAKE makes one fail
//! or answer otherwise.
//!
//! handing.rs is which PRs go to a job and which verdicts count, refreshes.rs
//! the lock file refreshes, and passes.rs what a pass tells the owner and
//! how it fails.

mod handing;
mod passes;
mod refreshes;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const SWEEP: &str = env!("CARGO_BIN_EXE_burrow-renovate-sweep");
const REPO: &str = "sam/r";
const LOCK: &str = "Lock file maintenance (cargo)";

/// Written once for every test: a file being written while another test
/// starts a process is open for writing in that process until it execs,
/// and running the file then fails with "Text file busy".
fn bin() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        for (name, text) in STAND_INS {
            let path = bin.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{text}")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin
    })
    .path()
}

const STAND_INS: [(&str, &str); 6] = [
    (
        // GitHub: the user's login (FAKE/user-fails to fail), a repo's
        // Renovate PRs (FAKE/prs-OWNER_REPO, or FAKE/prs for every repo),
        // PR N's comments (FAKE/comments-N) and Renovate's comments as REST
        // has them (FAKE/rest-N). It refuses an edit when FAKE/gh-refuses
        // is there.
        "gh",
        r#"echo "gh $*" >>"$FAKE/log"
case "$1 $2" in
  "api user") [ ! -e "$FAKE/user-fails" ] || exit 1; echo me ;;
  "pr list") cat "$FAKE/prs-$(echo "$4" | tr / _)" 2>/dev/null || cat "$FAKE/prs" 2>/dev/null || echo [] ;;
  "pr view") cat "$FAKE/comments-$3" 2>/dev/null || echo '{"comments": []}' ;;
  "api --paginate") n=${4%/comments}; cat "$FAKE/rest-${n##*/}" 2>/dev/null ;;
  "api -X") [ ! -e "$FAKE/gh-refuses" ] ;;
  "label create"|"pr edit") ;;
  *) echo "gh $*: not a stand-in answer" >&2; exit 1 ;;
esac
"#,
    ),
    (
        // A clone's origin, from DIR/.origin.
        "git",
        r#"[ "$1 $3 $4 $5" = "-C remote get-url origin" ] || exit 2
cat "$2/.origin"
"#,
    ),
    (
        "major-seal",
        r#"echo "major-seal $*" >>"$FAKE/log"
[ "$1" = --check ] && shift && grep -qxF -- "$*" "$FAKE/seals" 2>/dev/null
"#,
    ),
    (
        // Its limits (FAKE/limits, or the steward's own), or the time on the
        // line of the comments it reads that names whom it's asked about.
        "lock-wait",
        r#"if [ "$1" = --limits ]; then cat "$FAKE/limits" 2>/dev/null || echo 3 21600 3 600; exit; fi
echo "lock-wait $1" >>"$FAKE/log"
while read -r who when; do [ "$who" != "$1" ] || echo "$when"; done
"#,
    ),
    (
        // A job, kept whether or not it starts (FAKE/proj-fails).
        "proj",
        r#"n=$(ls "$FAKE" | grep -c '^job-'); printf '%s\0' "$@" >"$FAKE/job-$n"
[ ! -e "$FAKE/proj-fails" ] || exit 3
"#,
    ),
    (
        "notify",
        r#"n=$(ls "$FAKE" | grep -c '^notice-'); printf '%s\0' "$@" >"$FAKE/notice-$n"
[ ! -e "$FAKE/notify-fails" ] || exit 4
"#,
    ),
];

struct Machine {
    _dir: tempfile::TempDir,
    root: PathBuf,
    /// The machine's own App, as its name and its bot's login.
    app: Option<(&'static str, &'static str)>,
    /// The steward's repos.json, once there's one.
    covered: Option<PathBuf>,
}

/// One sweep: what it exited with and said, the jobs it started and the
/// notices it sent (each its words), and what the stand-ins noted.
struct Pass {
    code: Option<i32>,
    out: String,
    err: String,
    jobs: Vec<Vec<String>>,
    notices: Vec<Vec<String>>,
    log: Vec<String>,
}

impl Pass {
    /// Each job's task.
    fn tasks(&self) -> Vec<&str> {
        self.jobs
            .iter()
            .map(|j| j.last().unwrap().as_str())
            .collect()
    }

    /// The one job's task.
    fn task(&self) -> &str {
        assert_eq!(self.jobs.len(), 1, "{:#?}", self.jobs);
        self.tasks()[0]
    }

    /// The projects jobs were started in.
    fn projects(&self) -> Vec<&str> {
        let mut p: Vec<&str> = self.jobs.iter().map(|j| j[j.len() - 2].as_str()).collect();
        p.sort();
        p
    }

    /// The notice on `what`: whether it's opened or closed, and its body.
    fn notice(&self, what: &str) -> (&str, &str) {
        let n: Vec<&Vec<String>> = self.notices.iter().filter(|n| n[1] == what).collect();
        assert_eq!(n.len(), 1, "{:#?}", self.notices);
        (n[0][0].as_str(), n[0].last().unwrap().as_str())
    }

    fn majors(&self) -> (&str, &str) {
        self.notice("majors")
    }

    /// The titles it asked GitHub for.
    fn asked(&self) -> Vec<&str> {
        self.log
            .iter()
            .filter_map(|l| l.strip_prefix("gh api -X PATCH "))
            .collect()
    }
}

/// What asking for PR `n` to have `title` looks like.
fn retitled(n: u64, title: &str) -> String {
    format!("repos/{REPO}/pulls/{n} -f title={title}")
}

fn rebase(n: u64) -> Vec<String> {
    vec![retitled(n, &format!("rebase!{LOCK}"))]
}

fn unasked(n: u64, title: &str) -> Vec<String> {
    vec![retitled(n, title)]
}

impl Machine {
    fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        fs::create_dir(root.join("fake")).unwrap();
        fs::create_dir(root.join("projects")).unwrap();
        let m = Machine {
            _dir: dir,
            root,
            app: None,
            covered: None,
        };
        m.project("r", "https://github.com/sam/r.git");
        m
    }

    fn as_app() -> Machine {
        Machine {
            app: Some(("workers", "sam-workers")),
            ..Machine::new()
        }
    }

    fn path(&self, p: &str) -> PathBuf {
        self.root.join(p)
    }

    /// Its state dir.
    fn state(&self, p: &str) -> PathBuf {
        self.path("state").join(p)
    }

    /// A clone in its projects of `origin`.
    fn project(&self, name: &str, origin: &str) {
        let dir = self.path("projects").join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(".origin"), format!("{origin}\n")).unwrap();
    }

    /// FAKE/NAME, which makes a stand-in fail or answer otherwise.
    fn set(&self, name: &str, text: &str) {
        fs::write(self.path("fake").join(name), text).unwrap();
    }

    fn unset(&self, name: &str) {
        let _ = fs::remove_file(self.path("fake").join(name));
    }

    /// Every repo's open Renovate PRs.
    fn prs(&self, prs: &[Value]) {
        self.set("prs", &Value::from(prs.to_vec()).to_string());
    }

    /// PR `n`'s comments.
    fn comments(&self, n: u64, comments: &[Value]) {
        self.set(
            &format!("comments-{n}"),
            &json!({ "comments": comments }).to_string(),
        );
    }

    /// The job's verdict `v` on PR `n`'s head for `base`, by `by`, with a
    /// seal major-seal takes.
    fn verdict(&self, n: u64, v: &str, base: &str, by: &str) -> Value {
        let mac = format!("{:032x}", n * 1000 + u64::from(v == "ok"));
        let seals = self.path("fake/seals");
        let mut all = fs::read_to_string(&seals).unwrap_or_default();
        all.push_str(&format!("{REPO} {n} {v} {} {base} {mac}\n", sha(n)));
        fs::write(seals, all).unwrap();
        marked(by, &marker(n, v, base, &mac))
    }

    /// The steward's repos.json, written `age` seconds ago: the repos it
    /// covers, and the ones whose last pass failed, each with `proofs`.
    fn covered(&mut self, repos: &[&str], failed: &[&str], age: u64, proofs: Value) {
        let mut listed = serde_json::Map::new();
        for r in repos {
            listed.insert(r.to_string(), json!({"covered": true, "proofs": proofs}));
        }
        for r in failed {
            listed.insert(r.to_string(), json!({"covered": false, "proofs": proofs}));
        }
        let path = self.path("repos.json");
        let at = now() - age;
        fs::write(&path, json!({"at": at, "repos": listed}).to_string()).unwrap();
        self.covered = Some(path);
    }

    fn config(&self) -> PathBuf {
        let b = bin();
        let app = self
            .app
            .map(|(name, bot)| json!({"name": name, "bot": bot}));
        let config = json!({
            "name": "renovate-sweep", "state": self.path("state"), "projects": self.path("projects"),
            "owner": "Sam", "settled": "review-settled", "covered": self.covered, "app": app,
            "renovateRun": 7200,
            "tools": {"gh": b.join("gh"), "git": b.join("git"), "proj": b.join("proj"),
                      "notify": b.join("notify"), "seal": b.join("major-seal"),
                      "lockWait": b.join("lock-wait")},
        });
        let path = self.path("config.json");
        fs::write(&path, config.to_string()).unwrap();
        path
    }

    /// A sweep, of `words`.
    fn sweep(&self, words: &[&str]) -> Pass {
        let fake = self.path("fake");
        for e in fs::read_dir(&fake).unwrap() {
            let name = e.unwrap().file_name().into_string().unwrap();
            if name == "log" || name.starts_with("job-") || name.starts_with("notice-") {
                fs::remove_file(fake.join(name)).unwrap();
            }
        }
        let out = Command::new(SWEEP)
            .arg(self.config())
            .args(words)
            .env("FAKE", &fake)
            .output()
            .unwrap();
        let said = |prefix: &str| {
            let mut files: Vec<(usize, Vec<String>)> = fs::read_dir(&fake)
                .unwrap()
                .filter_map(|e| {
                    let name = e.unwrap().file_name().into_string().unwrap();
                    let n = name.strip_prefix(prefix)?.parse().unwrap();
                    let text = fs::read_to_string(fake.join(&name)).unwrap();
                    let words = text.split_terminator('\0').map(str::to_string).collect();
                    Some((n, words))
                })
                .collect();
            files.sort();
            files.into_iter().map(|(_, w)| w).collect()
        };
        Pass {
            code: out.status.code(),
            out: String::from_utf8_lossy(&out.stdout).into_owned(),
            err: String::from_utf8_lossy(&out.stderr).into_owned(),
            jobs: said("job-"),
            notices: said("notice-"),
            log: fs::read_to_string(fake.join("log"))
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect(),
        }
    }

    /// A full sweep that went as it should, as the notices are concerned.
    fn pass(&self) -> Pass {
        let p = self.sweep(&[]);
        assert_eq!(p.code, Some(0), "{}", p.err);
        assert_eq!(p.err, "");
        p
    }

    /// What PR `n`'s key says it was handed over for.
    fn key(&self, n: u64, suffix: &str) -> String {
        fs::read_to_string(self.state(&format!("sam_r-{n}{suffix}"))).unwrap_or_default()
    }

    fn keep(&self, n: u64, suffix: &str, text: &str) {
        fs::create_dir_all(self.path("state")).unwrap();
        fs::write(self.state(&format!("sam_r-{n}{suffix}")), text).unwrap();
    }

    /// The sweep's last request for PR `n` made `seconds` ago (its first
    /// `keep` words kept).
    fn since(&self, n: u64, seconds: u64, suffix: &str, keep: usize) {
        let was = self.key(n, suffix);
        let mut words: Vec<String> = was
            .split_whitespace()
            .take(keep)
            .map(str::to_string)
            .collect();
        words.push((now() - seconds).to_string());
        self.keep(n, suffix, &format!("{}\n", words.join(" ")));
    }

    fn asked_ago(&self, n: u64, seconds: u64) {
        self.since(n, seconds, "", 4);
    }

    /// The sweep's last taking back of PR `n`'s request, `seconds` ago.
    fn released(&self, n: u64, seconds: u64) {
        self.since(n, seconds, ".released", 2);
    }

    /// A lock file refresh lock-age refused for age only, till `ripe`, as
    /// Renovate (`by`) says on it; under `title`.
    fn refresh(&self, n: u64, ripe: u64, by: &str, title: &str) -> Value {
        self.set(&format!("rest-{n}"), &format!("{by} {ripe}\n"));
        refresh(n, title)
    }
}

/// A lock file refresh whose one red check is Renovate's artifacts.
fn refresh(n: u64, title: &str) -> Value {
    let mut p = pr_with(n, &[], "SUCCESS");
    p["title"] = title.into();
    p["headRefName"] = "renovate/cargo-lock-file-maintenance".into();
    p["statusCheckRollup"].as_array_mut().unwrap().push(json!({
        "__typename": "StatusContext", "context": "renovate/artifacts", "state": "FAILURE",
        "startedAt": "2099-01-01T00:00:00Z",
    }));
    p
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn sha(n: u64) -> String {
    format!("{n:040x}")
}

fn pr(n: u64) -> Value {
    pr_with(n, &["major"], "SUCCESS")
}

fn red(n: u64) -> Value {
    pr_with(n, &[], "FAILURE")
}

fn pr_with(n: u64, labels: &[&str], conclusion: &str) -> Value {
    let labels: Vec<Value> = labels.iter().map(|l| json!({ "name": l })).collect();
    json!({
        "number": n, "title": format!("Update foo{n} to v2"), "headRefName": format!("renovate/foo{n}-2.x"),
        "headRefOid": sha(n), "baseRefName": "main", "labels": labels, "createdAt": "2099-01-01T00:00:00Z",
        "statusCheckRollup": [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": conclusion}],
    })
}

/// PR `n` with these check runs: (status, conclusion) each.
fn ci(n: u64, runs: &[(&str, &str)]) -> Value {
    let mut p = pr(n);
    p["statusCheckRollup"] = runs
        .iter()
        .enumerate()
        .map(|(i, (s, c))| {
            json!({"__typename": "CheckRun", "name": format!("ci{i}"), "status": s, "conclusion": c})
        })
        .collect();
    p
}

/// `p` with its checks finished at `t`.
fn at(mut p: Value, t: &str) -> Value {
    for c in p["statusCheckRollup"].as_array_mut().unwrap() {
        c["completedAt"] = t.into();
    }
    p
}

/// `p` with `check` on its head too.
fn with(mut p: Value, check: Value) -> Value {
    p["statusCheckRollup"].as_array_mut().unwrap().push(check);
    p
}

fn marker(n: u64, v: &str, base: &str, mac: &str) -> String {
    format!("<!-- major-verdict: {v} {} on {base} {mac} -->", sha(n))
}

fn marked(by: &str, marker: &str) -> Value {
    json!({"author": {"login": by}, "body": format!("Read it.\n{marker}")})
}
