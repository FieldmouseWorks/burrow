//! advisory-watch as its unit runs it: the config file, a stand-in for
//! each program it runs (each keeps how it was run, in FAKE), the projects'
//! checkouts, and KEV's list on a local port.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock};

const BIN: &str = env!("CARGO_BIN_EXE_advisory-watch");

fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The stand-ins, written once for every test: a file being written while
/// another test starts a process is open for writing in that process until
/// it execs, and running the file then fails with "Text file busy".
fn stand_ins() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        let b = bin.path();
        fs::set_permissions(b, fs::Permissions::from_mode(0o755)).unwrap();
        // Runs it as it is, and never under itself.
        script(
            &b.join("sudo"),
            r#"#!/bin/sh
echo "$*" >>"$FAKE/sudo"
[ "$IN_SUDO" = "" ] && [ "$1" = -- ] || exit 9
shift
IN_SUDO=1 exec "$@"
"#,
        );
        script(
            &b.join("runuser"),
            r#"#!/bin/sh
[ "$1" = -u ] && [ "$3" = -- ] || exit 9
echo "$2 ${XDG_RUNTIME_DIR-unset} ${4##*/}" >>"$FAKE/runuser"
shift 3
exec "$@"
"#,
        );
        script(
            &b.join("nix-store"),
            r#"#!/bin/sh
echo "$*" >>"$FAKE/nix-store"
cat "$FAKE/store"
exit "$(cat "$FAKE/store-exit" 2>/dev/null || echo 0)"
"#,
        );
        script(
            &b.join("vulnix"),
            r#"#!/bin/sh
echo "$*" >>"$FAKE/vulnix"
[ ! -e "$FAKE/vulnix.json" ] || cat "$FAKE/vulnix.json"
echo noise >&2
exit "$(cat "$FAKE/vulnix-exit" 2>/dev/null || echo 0)"
"#,
        );
        script(
            &b.join("osv-scanner"),
            r#"#!/bin/sh
for d; do :; done
echo "$*" >>"$FAKE/osv-scanner"
[ ! -e "$d/osv.json" ] || cat "$d/osv.json"
echo noise >&2
exit "$(cat "$d/osv-exit" 2>/dev/null || echo 0)"
"#,
        );
        // Keeps each job's words and its task, and says its id.
        script(
            &b.join("proj"),
            r#"#!/bin/sh
if [ -e "$FAKE/proj-exit" ]; then echo "proj: no" >&2; exit "$(cat "$FAKE/proj-exit")"; fi
n=$(( $(cat "$FAKE/jobs" 2>/dev/null || echo 0) + 1 ))
echo "$n" >"$FAKE/jobs"
words=
i=0
for a; do
  i=$((i + 1))
  if [ "$i" -lt "$#" ]; then words="$words $a"; else printf '%s' "$a" >"$FAKE/job-$n.task"; fi
done
echo "${words# }" >"$FAKE/job-$n.words"
echo "job-$n"
"#,
        );
        bin
    })
    .path()
}

/// KEV's list on a local port: /NAME is the file NAME in `dir`, with the
/// status in NAME.status if there's one (and NAME.location as where it
/// moved to).
struct Kev {
    url: String,
    asked: Arc<Mutex<Vec<String>>>,
}

impl Kev {
    fn serve(dir: PathBuf) -> Kev {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let asked = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&asked);
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap_or("/").to_owned();
                loop {
                    let mut header = String::new();
                    if reader.read_line(&mut header).unwrap_or(0) == 0 || header == "\r\n" {
                        break;
                    }
                }
                kept.lock().unwrap().push(path.clone());
                let file = dir.join(path.trim_start_matches('/'));
                let status = fs::read_to_string(file.with_extension("status")).ok();
                let location = fs::read_to_string(file.with_extension("location"))
                    .map_or(String::new(), |l| format!("Location: {l}\r\n"));
                let (code, body) = match status {
                    Some(code) => (code.trim().to_owned(), fs::read(&file).unwrap_or_default()),
                    None => {
                        fs::read(&file).map_or(("404".into(), Vec::new()), |b| ("200".into(), b))
                    }
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 {code} X\r\n{location}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(&body);
            }
        });
        Kev { url, asked }
    }

    fn asked(&self) -> Vec<String> {
        self.asked.lock().unwrap().clone()
    }
}

fn kev_list(entries: &[(&str, &str, &str)]) -> String {
    let list: Vec<_> = entries
        .iter()
        .map(|(cve, vendor, product)| {
            serde_json::json!({"cveID": cve, "vendorProject": vendor, "product": product,
                "vulnerabilityName": format!("{product} bug"), "dateAdded": "2026-01-01"})
        })
        .collect();
    serde_json::json!({ "catalogVersion": "1", "vulnerabilities": list }).to_string()
}

fn osv_list(found: &[(&str, &str)]) -> String {
    let packages: Vec<_> = found
        .iter()
        .map(|(id, pkg)| {
            serde_json::json!({"package": {"name": pkg, "version": "1.0", "ecosystem": "crates.io"},
                "groups": [{"ids": [id], "max_severity": "7.5"}]})
        })
        .collect();
    serde_json::json!({"results": [{"source": {"path": "Cargo.lock"}, "packages": packages}]})
        .to_string()
}

struct Machine {
    dir: tempfile::TempDir,
    kev: Kev,
}

impl Machine {
    /// A machine with openssl and exim installed, its own project `box`, and
    /// `app`; KEV's list at the second place it looks, the first refusing.
    fn new() -> Machine {
        Machine::with_kev(&["first.json", "second.json"])
    }

    fn with_kev(places: &[&str]) -> Machine {
        stand_ins();
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for p in [
            "src/box/.git",
            "src/app/.git",
            "src/.hidden/.git",
            "src/plain",
            "src/wt",
            "fake",
            "www",
        ] {
            fs::create_dir_all(d.join(p)).unwrap();
        }
        fs::write(
            d.join("src/wt/.git"),
            "gitdir: /src/app/.git/worktrees/wt\n",
        )
        .unwrap();
        let hash = "0123456789abcdfghijklmnpqrsvwxyz";
        fs::write(
            d.join("fake/store"),
            format!("/nix/store/{hash}-openssl-3.0.15\n/nix/store/{hash}-exim-4.98\n/nix/store/{hash}-etc\n"),
        )
        .unwrap();
        let kev = Kev::serve(d.join("www"));
        // A refusal with a list in it isn't the list.
        fs::write(d.join("www/first.status"), "403").unwrap();
        fs::write(
            d.join("www/first.json"),
            kev_list(&[("CVE-5", "Exim", "Exim")]),
        )
        .unwrap();
        let places: Vec<String> = places.iter().map(|p| format!("{}/{p}", kev.url)).collect();
        let b = stand_ins();
        let config = serde_json::json!({
            "state": d.join("state"), "src": d.join("src"), "user": "sam", "project": "box",
            "host": "boxhost", "channel": "nixos-26.05", "update": "box-update", "flake": d.join("src/box"),
            "owner": "Sam", "notify": "box-notify", "boot": null, "train": false,
            "checks": ["rehearsal"], "system": "x86_64-linux", "app": "box-bot", "kev": places,
            "tools": {
                "sudo": b.join("sudo"), "runuser": b.join("runuser"), "nixStore": b.join("nix-store"),
                "vulnix": b.join("vulnix"), "osvScanner": b.join("osv-scanner"), "proj": b.join("proj"),
            },
        });
        fs::write(d.join("config.json"), config.to_string()).unwrap();
        let m = Machine { dir, kev };
        m.set_kev(&[
            ("CVE-1", "OpenSSL", "OpenSSL"),
            ("CVE-2", "Google", "Chrome"),
        ]);
        m.set_osv("box", &[]);
        m.set_osv("app", &[("RUSTSEC-1", "rustls")]);
        m.set_vulnix(r#"[{"pname": "exim", "name": "exim-4.98", "affected_by": ["CVE-9"], "cvssv3_basescore": {"CVE-9": 9.8}}]"#);
        m
    }

    fn path(&self, p: &str) -> PathBuf {
        self.dir.path().join(p)
    }

    fn set_kev(&self, entries: &[(&str, &str, &str)]) {
        fs::write(self.path("www/second.json"), kev_list(entries)).unwrap();
    }

    fn set_osv(&self, project: &str, found: &[(&str, &str)]) {
        fs::write(
            self.path(&format!("src/{project}/osv.json")),
            osv_list(found),
        )
        .unwrap();
    }

    fn set_vulnix(&self, json: &str) {
        fs::write(self.path("fake/vulnix.json"), json).unwrap();
    }

    /// It, as root starts it.
    fn command(&self, words: &[&str]) -> Command {
        let mut c = Command::new(BIN);
        c.arg(self.path("config.json"))
            .args(words)
            .env("FAKE", self.path("fake"))
            .env("XDG_RUNTIME_DIR", "/run/user/0")
            .env_remove("IN_SUDO");
        c
    }

    fn run(&self, words: &[&str]) -> Output {
        self.command(words).output().unwrap()
    }

    fn pass(&self) -> Output {
        let out = self.run(&[]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }

    fn fake(&self, name: &str) -> String {
        fs::read_to_string(self.path(&format!("fake/{name}"))).unwrap_or_default()
    }

    fn state(&self, name: &str) -> String {
        fs::read_to_string(self.path(&format!("state/{name}"))).unwrap_or_default()
    }

    /// Each job proj was asked for: its words, and its task.
    fn jobs(&self) -> Vec<(String, String)> {
        let n: usize = self.fake("jobs").trim().parse().unwrap_or(0);
        (1..=n)
            .map(|i| {
                let words = self.fake(&format!("job-{i}.words")).trim_end().to_owned();
                (words, self.fake(&format!("job-{i}.task")))
            })
            .collect()
    }

    fn seen(&self) -> Vec<String> {
        self.state("seen").lines().map(str::to_owned).collect()
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn root() -> bool {
    rustix::process::geteuid().is_root()
}

/// The log's lines without their dates.
fn undated(log: &str) -> Vec<String> {
    log.lines()
        .map(|l| {
            let (date, rest) = l.split_once(' ').unwrap();
            assert!(
                jiff::Timestamp::strptime("%Y-%m-%dT%H:%M:%S%:z", date).is_ok(),
                "{l}"
            );
            rest.to_owned()
        })
        .collect()
}

#[test]
fn a_first_pass_only_records_what_is_there() {
    let m = Machine::new();
    let out = m.pass();
    assert_eq!(stdout(&out), "");
    assert!(m.jobs().is_empty());
    assert_eq!(
        m.seen(),
        [
            "kev:CVE-1",
            "vulnix:CVE-9:exim-4.98",
            "osv:app:RUSTSEC-1:rustls"
        ]
    );
    for b in ["kev", "vulnix", "osv-app", "osv-box"] {
        assert!(m.path(&format!("state/baseline-{b}")).exists(), "{b}");
    }
    assert_eq!(m.state("log"), "");
    assert_eq!(m.fake("nix-store"), "-qR /run/current-system\n");
    assert_eq!(m.fake("vulnix"), "--system --json\n");
    // Checkouts of their own only, by name; as the user, without root's runtime dir.
    let src = m.path("src");
    assert_eq!(
        m.fake("osv-scanner"),
        format!(
            "scan source --recursive --format json {0}/app\nscan source --recursive --format json {0}/box\n",
            src.display()
        )
    );
    assert_eq!(
        m.fake("runuser"),
        "sam unset osv-scanner\nsam unset osv-scanner\n"
    );
    assert_eq!(m.kev.asked(), ["/first.json", "/second.json"]);
    // KEV's refusal at the first place isn't news when the second had it,
    // and what the scanners say on stderr is theirs.
    assert_eq!(stderr(&out), "");
    // Root starts it as it is; anyone else, through sudo (a_pass_started_as_anyone_else_goes_through_sudo).
    assert_eq!(m.fake("sudo").is_empty(), root());
}

#[test]
fn a_later_pass_hands_whats_new_to_each_projects_job() {
    let m = Machine::new();
    m.pass();
    m.set_kev(&[
        ("CVE-1", "OpenSSL", "OpenSSL"),
        ("CVE-3", "Exim", "Exim"),
        ("CVE-4", "Linux", "Kernel"),
    ]);
    m.set_osv("app", &[("RUSTSEC-1", "rustls"), ("RUSTSEC-2", "idna")]);
    m.set_osv("box", &[("RUSTSEC-3", "serde")]);
    m.pass();
    let jobs = m.jobs();
    assert_eq!(jobs.len(), 2, "{jobs:?}");
    assert_eq!(jobs[0].0, "run --first --app box-bot app");
    assert!(
        jobs[0].1.starts_with(
            "advisory-watch found new vulnerabilities in this project's dependencies (osv-scanner):\n\
             - RUSTSEC-2 in idna 1.0 (severity 7.5)\n\nRenovate opens"
        ),
        "{}",
        jobs[0].1
    );
    assert_eq!(jobs[1].0, "run --first --app box-bot box");
    assert!(
        jobs[1].1.starts_with(
            "advisory-watch found new vulnerabilities in what boxhost runs:\n\
             - CVE-3 (KNOWN EXPLOITED): Exim bug\n\
             - CVE-4 (KNOWN EXPLOITED): Kernel bug\n\
             - RUSTSEC-3 in serde 1.0 (severity 7.5)\n\nFor each one:"
        ),
        "{}",
        jobs[1].1
    );
    assert_eq!(
        undated(&m.state("log")),
        [
            "box\tCVE-3 (KNOWN EXPLOITED): Exim bug",
            "box\tCVE-4 (KNOWN EXPLOITED): Kernel bug",
            "app\tRUSTSEC-2 in idna 1.0 (severity 7.5)",
            "box\tRUSTSEC-3 in serde 1.0 (severity 7.5)",
            "app: handed to Claude, proj log job-1",
            "box: handed to Claude, proj log job-2",
        ]
    );
    // Seen as each job took it.
    assert!(m.seen().ends_with(&[
        "osv:app:RUSTSEC-2:idna".to_owned(),
        "kev:CVE-3".to_owned(),
        "kev:CVE-4".to_owned(),
        "osv:box:RUSTSEC-3:serde".to_owned(),
    ]));
    assert!(
        m.fake("runuser")
            .ends_with("sam unset proj\nsam unset proj\n")
    );
    // Nothing new, nothing handed over.
    m.pass();
    assert_eq!(m.jobs().len(), 2);
}

#[test]
fn a_job_that_cant_start_waits_for_the_next_pass() {
    let m = Machine::new();
    m.pass();
    m.set_kev(&[("CVE-3", "Exim", "Exim")]);
    fs::write(m.path("fake/proj-exit"), "1").unwrap();
    let out = m.pass();
    assert_eq!(
        stderr(&out),
        "proj: no\nadvisory-watch: box: proj run: exit status: 1\n"
    );
    assert!(m.jobs().is_empty());
    assert!(!m.seen().contains(&"kev:CVE-3".to_owned()));
    let report = stdout(&m.run(&["report"]));
    assert!(
        report.ends_with("\nNot handed to a job yet (each pass tries again):\n- box: CVE-3 (KNOWN EXPLOITED): Exim bug\n"),
        "{report}"
    );
    // Taken, though KEV no longer lists it; found once.
    fs::remove_file(m.path("fake/proj-exit")).unwrap();
    m.set_kev(&[]);
    m.pass();
    let jobs = m.jobs();
    assert_eq!(jobs.len(), 1);
    assert!(
        jobs[0]
            .1
            .contains("\n- CVE-3 (KNOWN EXPLOITED): Exim bug\n\n"),
        "{}",
        jobs[0].1
    );
    assert_eq!(
        undated(&m.state("log")),
        [
            "box\tCVE-3 (KNOWN EXPLOITED): Exim bug",
            "box: not handed to Claude (proj run: exit status: 1); the next pass tries again",
            "box: handed to Claude, proj log job-1",
        ]
    );
    assert!(m.seen().contains(&"kev:CVE-3".to_owned()));
    assert!(!stdout(&m.run(&["report"])).contains("Not handed"));
}

#[test]
fn a_project_whose_checkout_has_gone_drops_what_it_had_waiting() {
    let m = Machine::new();
    m.pass();
    m.set_osv("app", &[("RUSTSEC-2", "idna")]);
    fs::write(m.path("fake/proj-exit"), "1").unwrap();
    m.pass();
    fs::remove_file(m.path("fake/proj-exit")).unwrap();
    fs::remove_dir_all(m.path("src/app/.git")).unwrap();
    m.pass();
    assert!(m.jobs().is_empty());
    assert!(
        m.state("log")
            .ends_with(" app: its checkout is gone, so what it had waiting is dropped\n"),
        "{}",
        m.state("log")
    );
    // The machine's own project isn't dropped: its job is still asked for.
    m.set_kev(&[("CVE-3", "Exim", "Exim")]);
    fs::remove_dir_all(m.path("src/box/.git")).unwrap();
    m.pass();
    assert_eq!(m.jobs().len(), 1);
    assert_eq!(m.jobs()[0].0, "run --first --app box-bot box");
    // Back, it's new again.
    fs::create_dir(m.path("src/app/.git")).unwrap();
    m.pass();
    let jobs = m.jobs();
    assert_eq!(jobs.len(), 2);
    assert!(
        jobs[1]
            .1
            .contains("\n- RUSTSEC-2 in idna 1.0 (severity 7.5)\n"),
        "{}",
        jobs[1].1
    );
}

#[test]
fn vulnix_runs_once_a_day() {
    let m = Machine::new();
    m.pass();
    let day = m.state("vulnix-day");
    assert!(
        jiff::civil::Date::strptime("%Y-%m-%d\n", &day).is_ok(),
        "{day}"
    );
    m.set_vulnix(r#"[{"pname": "exim", "name": "exim-4.98", "affected_by": ["CVE-9", "CVE-10"], "cvssv3_basescore": {"CVE-9": 9.8, "CVE-10": 8.1}}]"#);
    m.pass();
    // Unless the day turned between the two.
    if m.state("vulnix-day") == day {
        assert_eq!(m.fake("vulnix"), "--system --json\n");
        assert!(m.jobs().is_empty());
    }
    fs::write(m.path("state/vulnix-day"), "2000-01-01\n").unwrap();
    m.pass();
    assert_ne!(m.state("vulnix-day"), "2000-01-01\n");
    let jobs = m.jobs();
    assert_eq!(jobs.len(), 1);
    assert!(
        jobs[0].1.contains("\n- CVE-10 in exim-4.98 (CVSS 8.1)\n\n"),
        "{}",
        jobs[0].1
    );
}

#[test]
fn what_a_source_found_counts_whatever_its_exit() {
    let m = Machine::new();
    fs::write(m.path("fake/vulnix-exit"), "2").unwrap();
    fs::write(m.path("src/app/osv-exit"), "1").unwrap();
    m.pass();
    assert!(m.seen().contains(&"vulnix:CVE-9:exim-4.98".to_owned()));
    assert!(m.seen().contains(&"osv:app:RUSTSEC-1:rustls".to_owned()));
}

#[test]
fn a_source_it_cant_read_is_passed_over_and_tried_again() {
    let m = Machine::with_kev(&["first.json", "moved.json"]);
    fs::write(m.path("www/moved.status"), "301").unwrap();
    fs::write(m.path("www/moved.location"), "/second.json").unwrap();
    fs::remove_file(m.path("fake/vulnix.json")).unwrap();
    fs::write(m.path("fake/vulnix-exit"), "1").unwrap();
    fs::write(m.path("src/app/osv.json"), "not json").unwrap();
    fs::remove_file(m.path("src/box/osv.json")).unwrap();
    fs::write(m.path("src/box/osv-exit"), "128").unwrap();
    let out = m.pass();
    let err = stderr(&out);
    for said in [
        "advisory-watch: KEV: http://127.0.0.1:",
        "/first.json: HTTP 403",
        "/moved.json: HTTP 301",
        "advisory-watch: vulnix: exit status: 1",
        "/src/app: its output doesn't read",
    ] {
        assert!(err.contains(said), "{said}: {err}");
    }
    // A checkout with nothing osv-scanner reads is no news.
    assert!(!err.contains("/src/box"), "{err}");
    // The redirect wasn't followed.
    assert_eq!(m.kev.asked(), ["/first.json", "/moved.json"]);
    assert_eq!(m.seen(), Vec::<String>::new());
    for b in ["kev", "vulnix", "osv-app", "osv-box"] {
        assert!(!m.path(&format!("state/baseline-{b}")).exists(), "{b}");
    }
    assert!(!m.path("state/vulnix-day").exists());
    // Once they can be read, it's their first pass.
    fs::remove_file(m.path("www/moved.status")).unwrap();
    fs::copy(m.path("www/second.json"), m.path("www/moved.json")).unwrap();
    m.set_vulnix("[]");
    fs::remove_file(m.path("fake/vulnix-exit")).unwrap();
    m.set_osv("app", &[("RUSTSEC-1", "rustls")]);
    m.pass();
    assert!(m.jobs().is_empty());
    assert_eq!(m.seen(), ["kev:CVE-1", "osv:app:RUSTSEC-1:rustls"]);
}

#[test]
fn a_pass_started_as_anyone_else_goes_through_sudo() {
    let m = Machine::new();
    let mut c = m.command(&[]);
    // From a shell that came from sudo (sudo -iu dev) too.
    c.env("SUDO_UID", "1000");
    if root() {
        // As nobody, who may write the state and read the rest.
        for p in ["", "state", "src", "fake", "www"] {
            fs::create_dir_all(m.path(p)).unwrap();
            fs::set_permissions(m.path(p), fs::Permissions::from_mode(0o777)).unwrap();
        }
        c.uid(65534).gid(65534);
    }
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(
        m.fake("sudo"),
        format!(
            "-- {BIN} {} __through-sudo\n",
            m.path("config.json").display()
        )
    );
    assert_eq!(m.seen().len(), 3);
}

#[test]
fn one_pass_at_a_time() {
    let m = Machine::new();
    let held = advisory_watch::state::lock(&m.path("state")).unwrap();
    let out = m.pass();
    assert_eq!(
        stdout(&out),
        "advisory-watch: another pass is running, so this one does nothing\n"
    );
    assert_eq!(m.fake("nix-store"), "");
    drop(held);
    m.pass();
    assert_eq!(m.seen().len(), 3);
}

#[test]
fn a_report_only_reads() {
    let m = Machine::new();
    let out = m.run(&["report"]);
    assert!(out.status.success());
    assert_eq!(stdout(&out), "nothing seen yet\n");
    assert!(!m.path("state").exists());
    assert_eq!(m.fake("sudo"), "");
    m.pass();
    m.set_kev(&[("CVE-3", "Exim", "Exim")]);
    m.pass();
    let full = m
        .command(&["report"])
        .stdout(fs::File::create("/dev/full").unwrap())
        .output()
        .unwrap();
    assert_eq!(full.status.code(), Some(1));
    assert!(
        stderr(&full).starts_with("advisory-watch: stdout: "),
        "{}",
        stderr(&full)
    );
    let out = stdout(&m.run(&["report"]));
    assert_eq!(
        undated(&out),
        [
            "box\tCVE-3 (KNOWN EXPLOITED): Exim bug",
            "box: handed to Claude, proj log job-1"
        ]
    );
}

#[test]
fn what_fails_a_pass() {
    let m = Machine::new();
    fs::write(m.path("fake/store-exit"), "1").unwrap();
    let out = m.run(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stderr(&out).contains("advisory-watch: nix-store -qR /run/current-system: exit status: 1")
    );
    assert_eq!(m.fake("vulnix"), "");
    fs::remove_file(m.path("fake/store-exit")).unwrap();
    // State it can't keep.
    fs::remove_dir_all(m.path("state")).unwrap();
    fs::write(m.path("state"), "").unwrap();
    let out = m.run(&[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("/state"), "{}", stderr(&out));
}

#[test]
fn usage() {
    let m = Machine::new();
    for words in [&["report", "now"][..], &["status"], &["--report"]] {
        let out = m.run(words);
        assert_eq!(out.status.code(), Some(2), "{words:?}");
        assert_eq!(stderr(&out), "usage: advisory-watch [report]\n");
    }
    let out = Command::new(BIN).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stderr(&out), "usage: advisory-watch CONFIG [report]\n");
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(m.path("config.json")).unwrap()).unwrap();
    config["extra"] = 1.into();
    fs::write(m.path("config.json"), config.to_string()).unwrap();
    let out = m.run(&["report"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("unknown field `extra`"),
        "{}",
        stderr(&out)
    );
    assert_eq!(m.fake("nix-store"), "");
    assert_eq!(m.fake("sudo"), "");
}
