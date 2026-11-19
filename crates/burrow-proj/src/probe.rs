//! Read-only host answers. Jobs send grammar, never commands or credentials.
use crate::{config::Config, guest, job::Job, scratch};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub(crate) fn socket(cfg: &Config, job: &Job, n: &str) -> PathBuf {
    scratch::dir(cfg, job.id).join(format!("probe-{n}.sock"))
}

// Resolve through an open directory rather than putting the full scratch
// path in sockaddr_un's small sun_path. The socket still lives only in this
// job's scratch and remains subject to the same filesystem permissions.
fn address(path: &Path) -> Result<(fs::File, PathBuf), String> {
    let directory = fs::File::open(path.parent().ok_or("probe socket has no parent")?)
        .map_err(|e| e.to_string())?;
    let name = path.file_name().ok_or("probe socket has no name")?;
    let address = PathBuf::from(format!("/proc/self/fd/{}", directory.as_raw_fd())).join(name);
    Ok((directory, address))
}

/// The ordinary job client runs before loading any host config.
pub fn client(path: &Path, words: &[String]) -> Result<(), String> {
    print!("{}", exchange(path, words)?);
    Ok(())
}
fn exchange(path: &Path, words: &[String]) -> Result<String, String> {
    result(&request(path, words.join(" ").as_bytes(), 65536)?)
}

/// Shared bounded Unix transport; each endpoint supplies its own grammar.
pub(crate) fn request(path: &Path, request: &[u8], max: u64) -> Result<String, String> {
    let (_directory, address) = address(path)?;
    let mut stream = UnixStream::connect(address).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream.write_all(request).map_err(|e| e.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;
    let mut answer = String::new();
    stream
        .take(max + 1)
        .read_to_string(&mut answer)
        .map_err(|e| e.to_string())?;
    if answer.len() as u64 > max {
        return Err(format!("host response exceeded {max} bytes"));
    }
    Ok(answer)
}

fn result(answer: &str) -> Result<String, String> {
    let answer: Result<String, String> = serde_json::from_str(answer).map_err(|e| e.to_string())?;
    answer
}

/// The VM has no host sockets. Its only request is a bounded text file.
#[derive(Serialize, Deserialize)]
struct Request {
    id: i64,
    question: String,
}
#[derive(Serialize, Deserialize)]
struct Response {
    id: i64,
    answer: String,
}
pub fn guest_client(path: &Path, words: &[String]) -> Result<(), String> {
    print!("{}", guest_exchange(path, words)?);
    Ok(())
}
fn guest_exchange(path: &Path, words: &[String]) -> Result<String, String> {
    let request = path.with_extension("probe");
    let response = path.with_extension("answer");
    let lock = fs::File::options()
        .create(true)
        .append(true)
        .open(path.with_extension("probe-lock"))
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let _ = fs::remove_file(&response);
    let id = crate::now_nanos();
    let question = words.join(" ");
    burrow_probe::query::Query::parse(&question)?;
    let pending = path.with_extension("probe-next");
    fs::write(
        &pending,
        serde_json::to_vec(&Request { id, question }).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(pending, &request).map_err(|e| e.to_string())?;
    let until = Instant::now() + Duration::from_secs(180);
    loop {
        if let Ok(bytes) = fs::read(&response)
            && let Ok(response) = serde_json::from_slice::<Response>(&bytes)
            && response.id == id
        {
            return result(&response.answer);
        }
        if Instant::now() >= until {
            return Err("host probe timed out".into());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn answer(cfg: &Config, question: &str) -> String {
    let answer = (|| {
        let mut probe = cfg
            .probe
            .clone()
            .ok_or("the host has no probe configured")?;
        probe.proj = Some(cfg.me.clone());
        let q = burrow_probe::query::Query::parse(question)?;
        let uid = fs::read_to_string("/proc/self/status")
            .map_err(|e| e.to_string())?
            .lines()
            .find_map(|l| {
                l.strip_prefix("Uid:")
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse().ok())
            })
            .ok_or("no host uid")?;
        let machine = burrow_probe::answer::Machine {
            cfg: &probe,
            root: "/".into(),
            now: crate::now(),
            uid,
            run: &burrow_probe::answer::Commands,
        };
        machine.answer(&q)
    })();
    // Errors can contain journal or command output too.
    let clean = |s: String| {
        let scrubbed = burrow_probe::scrub::scrub(&s);
        let mut text: String = scrubbed.as_str().chars().take(6000).collect();
        if text.len() < scrubbed.as_str().len() {
            text.push_str("\n[answer truncated]\n");
        }
        text
    };
    serde_json::to_string(&answer.map(clean).map_err(clean)).expect("probe answer")
}

/// Host transports kept alive for one engine invocation. The VM gets only
/// the read-only probe mailbox; local jobs also get the host recovery socket.
pub struct Bridge {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Bridge {
    pub fn start(cfg: &Config, job: &Job, n: &str, agent: Option<&str>) -> Result<Self, String> {
        Self::start_with(cfg, job, n, agent, PathBuf::from("agent"))
    }
    fn start_with(
        cfg: &Config,
        job: &Job,
        n: &str,
        agent: Option<&str>,
        executable: PathBuf,
    ) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let done = stop.clone();
        let cfg = cfg.clone();
        let path = socket(&cfg, job, n);
        fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
        let _ = fs::remove_file(&path);
        let (_directory, address) = address(&path)?;
        let listener = UnixListener::bind(address).map_err(|e| e.to_string())?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        // Host jobs have a separate endpoint: the probe grammar stays read-only.
        let jobs_path = crate::host_jobs::socket(&cfg, job, n);
        let jobs_listener = if agent.is_none() {
            let _ = fs::remove_file(&jobs_path);
            let (_directory, address) = self::address(&jobs_path)?;
            let listener = UnixListener::bind(address).map_err(|e| e.to_string())?;
            listener.set_nonblocking(true).map_err(|e| e.to_string())?;
            Some(listener)
        } else {
            None
        };
        let uid = rustix::process::geteuid().as_raw();
        let vm = agent.map(|a| (a.to_owned(), PathBuf::from(guest::path(job, n))));
        let thread = thread::spawn(move || {
            while !done.load(Ordering::Relaxed) {
                if let Some((agent, path)) = &vm {
                    let request = path.with_extension("probe");
                    let response = path.with_extension("answer");
                    let got = guest::bounded(
                        Command::new(&executable).args([
                            "sh",
                            agent,
                            "sh",
                            "-c",
                            "if [ -f \"$1\" ]; then head -c 4096 -- \"$1\"; rm -f -- \"$1\"; fi",
                            "sh",
                            &request.to_string_lossy(),
                        ]),
                        Duration::from_secs(30),
                    );
                    if let Ok(bytes) = got
                        && !bytes.is_empty()
                    {
                        let Ok(request) = serde_json::from_slice::<Request>(&bytes) else {
                            continue;
                        };
                        let text = serde_json::to_string(&Response {
                            id: request.id,
                            answer: answer(&cfg, &request.question),
                        })
                        .expect("probe response");
                        let _ = guest::bounded(
                            Command::new(&executable).args([
                                "sh",
                                agent,
                                "sh",
                                "-c",
                                "printf '%s' \"$2\" > \"$1.next\" && mv -- \"$1.next\" \"$1\"",
                                "sh",
                                &response.to_string_lossy(),
                                &text,
                            ]),
                            Duration::from_secs(30),
                        );
                    }
                } else {
                    serve(&listener, 4096, uid, |request| answer(&cfg, request));
                    if let Some(listener) = &jobs_listener {
                        serve(listener, crate::host_jobs::MAX_REQUEST, uid, |request| {
                            crate::host_jobs::answer(&cfg, request)
                        });
                    }
                }
                thread::sleep(Duration::from_millis(if vm.is_some() { 1000 } else { 100 }));
            }
            let _ = fs::remove_file(path);
            let _ = fs::remove_file(jobs_path);
        });
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

fn serve(listener: &UnixListener, max: u64, uid: u32, answer: impl FnOnce(&str) -> String) {
    if let Ok((mut stream, _)) = listener.accept() {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        // The kernel authenticates the caller; no chmod of a pathname in
        // writable scratch, and no uid supplied in the request, is trusted.
        if !rustix::net::sockopt::socket_peercred(&stream)
            .is_ok_and(|peer| peer.uid.as_raw() == uid)
        {
            let denied =
                serde_json::to_string(&Err::<String, _>("host transport refuses a different user"))
                    .unwrap();
            let _ = stream.write_all(denied.as_bytes());
            return;
        }
        let mut request = String::new();
        if (&mut stream)
            .take(max + 1)
            .read_to_string(&mut request)
            .is_ok()
        {
            let response = if request.len() as u64 > max {
                serde_json::to_string(&Err::<String, _>("host request too large")).unwrap()
            } else {
                answer(&request)
            };
            let _ = stream.write_all(response.as_bytes());
        }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_foreign_peer_is_refused_before_dispatch_even_on_a_reachable_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("host.sock");
        let (_directory, address) = address(&path).unwrap();
        let listener = UnixListener::bind(&address).unwrap();
        let mut stream = UnixStream::connect(&address).unwrap();
        stream.write_all(b"[\"stop\",\"target\"]").unwrap();
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let foreign_owner = rustix::process::geteuid().as_raw().wrapping_add(1);
        serve(&listener, 4096, foreign_owner, |_| {
            panic!("foreign request dispatched")
        });
        let mut response = String::new();
        // A refusal may reset the unread request; the denial itself is still delivered.
        let _ = stream.read_to_string(&mut response);
        assert!(result(&response).unwrap_err().contains("different user"));
    }
    fn fixture(dir: &Path) -> Config {
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.jobs = dir.join("jobs");
        cfg.scratch = dir.join("scratch");
        cfg.probe = Some(serde_json::from_value(serde_json::json!({
            "repo":"sam/machine", "label":"probe", "authors":[], "host":"host", "user":"dev", "stateDir":dir,
            "journal":{"units":[], "userUnits":[]}, "proj":null,
            "tools":{"journalctl":"false", "systemctl":"false"}
        })).unwrap());
        cfg
    }
    #[test]
    fn sandbox_usage_answers_do_not_follow_host_secret_symlinks() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut cfg = fixture(root);
        cfg.probe.as_mut().unwrap().jobs = Some(cfg.jobs.clone());
        let job = Job::new(&cfg, "local");
        fs::create_dir_all(&job.dir).unwrap();
        fs::write(root.join("private"), "ordinary confidential text").unwrap();
        symlink(root.join("private"), job.path("usage-kind")).unwrap();
        fs::write(job.turn("1", "started"), crate::now().to_string()).unwrap();
        fs::write(job.turn("1", "jsonl"), "{}\n").unwrap();
        let _bridge = Bridge::start(&cfg, &job, "1", None).unwrap();
        let got = exchange(&socket(&cfg, &job, "1"), &["usage -5h".into()]).unwrap();
        assert!(got.contains("claude: 1 turns"), "{got}");
        assert!(!got.contains("ordinary confidential text"));
    }
    #[test]
    fn local_job_gets_host_answers_and_invalid_queries_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fixture(dir.path());
        let job = Job::new(&cfg, "local");
        let _bridge = Bridge::start(&cfg, &job, "1", None).unwrap();
        let expected = result(&answer(&cfg, "memory")).unwrap();
        let got = exchange(&socket(&cfg, &job, "1"), &["memory".into()]).unwrap();
        // Memory moves, but both are answers from the host, with its vocabulary.
        assert_eq!(
            got.split(" total,").next(),
            expected.split(" total,").next()
        );
        assert!(exchange(&socket(&cfg, &job, "1"), &["delete".into()]).is_err());
    }
    #[test]
    fn long_scratch_paths_do_not_limit_the_probe() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fixture(dir.path());
        cfg.scratch = dir.path().join("x".repeat(140));
        let job = Job::new(&cfg, "long");
        let _bridge = Bridge::start(&cfg, &job, "1", None).unwrap();
        let got = exchange(&socket(&cfg, &job, "1"), &["memory".into()]).unwrap();
        assert!(got.starts_with("RAM "));
    }
    #[test]
    fn host_journal_answers_are_scrubbed_before_crossing_the_bridge() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fixture(dir.path());
        let journal = dir.path().join("journalctl");
        let token = format!("ghp_{}", "x".repeat(36));
        burrow_test::executable(
            &journal,
            format!("#!/bin/sh\necho 'Authorization: Bearer {token}'\n"),
        );
        let probe = cfg.probe.as_mut().unwrap();
        probe.tools.journalctl = journal;
        probe.journal.units = vec!["test.service".into()];
        let job = Job::new(&cfg, "scrub");
        let _bridge = Bridge::start(&cfg, &job, "1", None).unwrap();
        let got = exchange(
            &socket(&cfg, &job, "1"),
            &["journal".into(), "test".into(), "-1h".into(), "now".into()],
        )
        .unwrap();
        assert!(!got.contains(&token));
        assert!(got.contains("[redacted]"));
    }
    #[test]
    fn sandbox_gets_scrubbed_host_crash_metadata_without_core_access() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fixture(dir.path());
        let journal = dir.path().join("journalctl");
        let token = format!("ghp_{}", "x".repeat(36));
        let entry = serde_json::json!({
            "COREDUMP_EXE":"/bin/example", "COREDUMP_PID":"123",
            "COREDUMP_USER_UNIT":"proj-example.service",
            "MESSAGE":format!("Stack trace of thread 123:\n#0 abort\nAuthorization: Bearer {token}"),
            "COREDUMP_ENVIRON":"plain private environment"
        });
        burrow_test::executable(
            &journal,
            format!("#!/bin/sh\ncat <<'ENTRY'\n{entry}\nENTRY\n"),
        );
        cfg.probe.as_mut().unwrap().tools.journalctl = journal;
        let job = Job::new(&cfg, "crashes");
        let _bridge = Bridge::start(&cfg, &job, "1", None).unwrap();
        let got = exchange(
            &socket(&cfg, &job, "1"),
            &["crashes /bin/example -1h".into()],
        )
        .unwrap();
        assert!(got.contains("COREDUMP_PID: 123"));
        assert!(got.contains("proj-example.service"));
        assert!(got.contains("#0 abort"));
        assert!(got.contains("[redacted]"));
        assert!(!got.contains(&token));
        assert!(!got.contains("plain private environment"));
    }
    #[test]
    fn crash_bridge_scrubs_complete_fields_before_output_limits() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fixture(dir.path());
        let journal = dir.path().join("journalctl");
        cfg.probe.as_mut().unwrap().tools.journalctl = journal.clone();
        let job = Job::new(&cfg, "crash-boundaries");
        let _bridge = Bridge::start(&cfg, &job, "1", None).unwrap();
        for (field, limit) in [("COREDUMP_CGROUP", 1024), ("MESSAGE", 12000)] {
            for token in [false, true] {
                let (value, hidden) = if token {
                    let credential = format!("gh{}_{}", "p", "x".repeat(36));
                    (
                        format!(
                            "Authorization: {}\n{credential} end",
                            "x".repeat(limit - 4 - 19 - 16)
                        ),
                        credential[..23].to_string(),
                    )
                } else {
                    let credential = "reader:opaque-value";
                    let url = format!("https://{credential}");
                    (
                        format!(
                            "Authorization: {}\n{url}@example.org/path",
                            "x".repeat(limit - url.len() - 16)
                        ),
                        credential.to_string(),
                    )
                };
                let mut entry =
                    serde_json::json!({"COREDUMP_EXE":"/bin/example","COREDUMP_PID":"123"});
                entry[field] = serde_json::json!(value);
                burrow_test::executable(
                    &journal,
                    format!("#!/bin/sh\ncat <<'ENTRY'\n{entry}\nENTRY\n"),
                );
                let got = exchange(
                    &socket(&cfg, &job, "1"),
                    &["crashes /bin/example -1h".into()],
                )
                .unwrap();
                assert!(
                    !got.contains(&hidden),
                    "{field}: credential crossed sandbox boundary"
                );
                assert!(got.contains("[redacted]") || got.contains("[answer truncated]"));
            }
        }
    }
    /// A gh that answers every GraphQL read with `reply`, as `gh api -i`
    /// does.
    fn gh(dir: &Path, reply: &serde_json::Value) -> PathBuf {
        let gh = dir.join("gh");
        burrow_test::executable(
            &gh,
            format!(
                "#!/bin/sh\ncat >/dev/null\nprintf 'HTTP/2.0 200 OK\\r\\nContent-Type: application/json\\r\\n\\r\\n'\ncat <<'REPLY'\n{reply}\nREPLY\n"
            ),
        );
        gh
    }
    #[test]
    fn issue_answers_cross_the_bridge_as_scrubbed_metadata_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fixture(dir.path());
        let token = format!("ghp_{}", "x".repeat(36));
        let who = serde_json::json!({"login":"sam","__typename":"User","databaseId":1});
        let reply = serde_json::json!({"data":{"repository":{"issue":{
            "state":"OPEN","createdAt":"2026-11-01T00:00:00Z",
            "title":"PRIVATE TITLE","body":"PRIVATE BODY",
            "author":who,
            "labels":{"totalCount":1,"nodes":[{"name":format!("label {token}")}]},
            "userContentEdits":{"totalCount":100,"nodes":vec![serde_json::json!({"editor":who,"diff":"PRIVATE EDIT"}); 100]},
            "timelineItems":{"totalCount":0,"nodes":[]}
        }}}});
        cfg.probe.as_mut().unwrap().tools.gh = Some(gh(dir.path(), &reply));
        let first = cfg.clone();
        let job = Job::new(&first, "issue");
        let _bridge = Bridge::start(&first, &job, "1", None).unwrap();
        let ask = |words: &[&str]| {
            exchange(
                &socket(&first, &job, "1"),
                &words.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
            )
        };
        let got = ask(&["issue", "sam/box", "210"]).unwrap();
        assert!(got.contains("at GitHub's cap of 100"), "{got}");
        assert!(
            got.contains("- sam (User 1): opened it, 100 edits"),
            "{got}"
        );
        assert!(got.contains("[redacted]") && !got.contains(&token), "{got}");
        assert!(!got.contains("PRIVATE"), "{got}");

        // GitHub's error, with a credential in it, is scrubbed too.
        let failing =
            serde_json::json!({"errors":[{"message":format!("Authorization: Bearer {token}")}]});
        cfg.probe.as_mut().unwrap().tools.gh = Some(gh(dir.path(), &failing));
        let job = Job::new(&cfg, "issue-error");
        let _bridge = Bridge::start(&cfg, &job, "1", None).unwrap();
        let err = exchange(
            &socket(&cfg, &job, "1"),
            &["issue".into(), "sam/box".into(), "210".into()],
        )
        .unwrap_err();
        assert!(err.starts_with("GitHub didn't say: "), "{err}");
        assert!(err.contains("[redacted]") && !err.contains(&token), "{err}");

        // Arguments outside the grammar never reach the host.
        for bad in [
            &["issue", "sam/box", "0"][..],
            &["issue", "sam/box", "1;id"],
            &["runners", "a", "b"],
            &["trust", "--write"],
            &["stop", "job"],
        ] {
            assert!(ask(bad).is_err(), "{bad:?}");
        }
    }
    #[test]
    fn vm_mailbox_returns_host_trust_and_runners_like_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fixture(dir.path());
        let probe = cfg.probe.as_mut().unwrap();
        probe.trust = Some(
            serde_json::from_value(serde_json::json!({
                "inbox": {"authors": [{"login": "sam", "id": 1}], "vm": false},
                "apps": [{"name": "workers", "login": "workers[bot]", "id": 3}],
                "machinery": "workers"
            }))
            .unwrap(),
        );
        probe.runners = vec![
            serde_json::from_value(serde_json::json!({
                "name": "ci-runner", "kind": "named", "repos": ["sam/box"], "labels": ["box"],
                "defined": true, "user": "ci", "environment": {"PATH": "/nonexistent/bin"}
            }))
            .unwrap(),
        ];
        let id = format!(
            "probe-facts-{}",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        let job = Job::new(&cfg, &id);
        let executable = dir.path().join("agent");
        burrow_test::executable(
            &executable,
            "#!/bin/sh\n[ \"$1\" = sh ] || exit 1\nshift 2\nexec \"$@\"\n",
        );
        let path = PathBuf::from(guest::path(&job, "1"));
        let bridge = Bridge::start_with(&cfg, &job, "1", Some("vm"), executable).unwrap();
        let trust = guest_exchange(&path, &["trust".into()]).unwrap();
        assert!(
            trust.contains("- a Codex job with no --app: the App workers[bot] (3)"),
            "{trust}"
        );
        let runners = guest_exchange(&path, &["runners".into()]).unwrap();
        assert!(
            runners.contains("ci-runner: a named runner for sam/box"),
            "{runners}"
        );
        assert!(
            runners.contains("default toolchain: none: neither rustc nor rustup is on PATH"),
            "{runners}"
        );
        drop(bridge);
        for extension in ["probe", "answer", "probe-lock"] {
            let _ = fs::remove_file(path.with_extension(extension));
        }
    }
    #[test]
    fn github_errors_are_scrubbed_whole_before_the_cut_on_both_transports() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = fixture(dir.path());
        let token = format!("ghp_{}", "a1B2c3D4e5".repeat(4));
        let key = format!("-----BEGIN RSA {} KEY-----", "PRIVATE");
        let end = format!("-----END RSA {} KEY-----", "PRIVATE");
        // Each credential straddles the 500-character cut.
        let said = [
            (token.clone(), format!("{}{token} after", "x".repeat(490))),
            (
                "hunter2hunter2".to_string(),
                format!("{}https://ci:hunter2hunter2@example.com/x", "y".repeat(480)),
            ),
            // Past what is kept whole at all.
            (
                "a1B2c3D4e5a1".to_string(),
                format!("{}{token}", "k".repeat(70_000)),
            ),
            (
                "MIIEvQIBADANBgkqhkiG".to_string(),
                format!(
                    "{}{key}\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\n{end}",
                    "z".repeat(480)
                ),
            ),
        ];
        let id = format!(
            "probe-cut-{}",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        for (n, (secret, text)) in said.iter().enumerate() {
            let failing = serde_json::json!({"errors":[{"message":text}]});
            cfg.probe.as_mut().unwrap().tools.gh = Some(gh(dir.path(), &failing));
            let words: Vec<String> = ["issue", "sam/box", "210"].map(String::from).to_vec();
            // The host socket.
            let socket_id = format!("{id}-s{n}");
            let job = Job::new(&cfg, &socket_id);
            let _bridge = Bridge::start(&cfg, &job, "1", None).unwrap();
            let err = exchange(&socket(&cfg, &job, "1"), &words).unwrap_err();
            assert!(
                !err.contains(&secret[..12]) && !err.contains("hunter2"),
                "{err}"
            );
            // The VM mailbox.
            let mailbox_id = format!("{id}-m{n}");
            let job = Job::new(&cfg, &mailbox_id);
            let executable = dir.path().join("agent");
            burrow_test::executable(
                &executable,
                "#!/bin/sh\n[ \"$1\" = sh ] || exit 1\nshift 2\nexec \"$@\"\n",
            );
            let path = PathBuf::from(guest::path(&job, "1"));
            let bridge = Bridge::start_with(&cfg, &job, "1", Some("vm"), executable).unwrap();
            let err = guest_exchange(&path, &words).unwrap_err();
            assert!(
                !err.contains(&secret[..12]) && !err.contains("hunter2"),
                "{err}"
            );
            drop(bridge);
            for extension in ["probe", "answer", "probe-lock"] {
                let _ = fs::remove_file(path.with_extension(extension));
            }
        }
    }
    #[test]
    fn vm_mailbox_returns_host_deployed_result_without_config_or_credentials() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = fixture(dir.path());
        let id = format!(
            "probe-test-{}",
            dir.path().file_name().unwrap().to_string_lossy()
        );
        let job = Job::new(&cfg, &id);
        let executable = dir.path().join("agent");
        burrow_test::executable(
            &executable,
            "#!/bin/sh\n[ \"$1\" = sh ] || exit 1\nshift 2\nexec \"$@\"\n",
        );
        let path = PathBuf::from(guest::path(&job, "1"));
        let expected = result(&answer(&cfg, "deployed"));
        let bridge = Bridge::start_with(&cfg, &job, "1", Some("vm"), executable).unwrap();
        let got = guest_exchange(&path, &["deployed".into()]);
        assert_eq!(got, expected);
        drop(bridge);
        for extension in ["probe", "answer", "probe-lock"] {
            let _ = fs::remove_file(path.with_extension(extension));
        }
    }
}
