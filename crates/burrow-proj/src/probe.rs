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
pub(crate) fn address(path: &Path) -> Result<(fs::File, PathBuf), String> {
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
    let (_directory, address) = address(path)?;
    let mut stream = UnixStream::connect(address).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(words.join(" ").as_bytes())
        .map_err(|e| e.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;
    let mut answer = String::new();
    stream
        .take(65536)
        .read_to_string(&mut answer)
        .map_err(|e| e.to_string())?;
    result(&answer)
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

pub(crate) struct Bridge {
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}
impl Bridge {
    pub(crate) fn start(
        cfg: &Config,
        job: &Job,
        n: &str,
        agent: Option<&str>,
    ) -> Result<Self, String> {
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
        let vm = agent.map(|a| (a.to_owned(), PathBuf::from(guest::path(job, n))));
        // The host's jobs, beside its answers: a VM's guest has no socket of
        // the host's to reach them by, and its proj says so.
        let jobs_path = crate::host_jobs::socket(&cfg, job, n);
        let jobs = match vm {
            Some(_) => None,
            None => {
                let _ = fs::remove_file(&jobs_path);
                let (_held, at) = self::address(&jobs_path)?;
                let jobs = UnixListener::bind(at).map_err(|e| e.to_string())?;
                jobs.set_nonblocking(true).map_err(|e| e.to_string())?;
                Some(jobs)
            }
        };
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
                } else if let Ok((mut stream, _)) = listener.accept() {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let mut request = String::new();
                    if (&mut stream)
                        .take(4096)
                        .read_to_string(&mut request)
                        .is_ok()
                    {
                        let _ = stream.write_all(answer(&cfg, &request).as_bytes());
                    }
                }
                // Each on its own, so a slow `proj stop` holds up no probe.
                if let Some(jobs) = &jobs
                    && let Ok((stream, _)) = jobs.accept()
                {
                    let cfg = cfg.clone();
                    thread::spawn(move || crate::host_jobs::serve(&cfg, stream));
                }
                thread::sleep(Duration::from_millis(if vm.is_some() { 1000 } else { 100 }));
            }
            let _ = fs::remove_file(path);
            if jobs.is_some() {
                let _ = fs::remove_file(jobs_path);
            }
        });
        Ok(Self {
            stop,
            thread: Some(thread),
        })
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
