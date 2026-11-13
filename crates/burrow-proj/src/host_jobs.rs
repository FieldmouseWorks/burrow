//! Other jobs, from inside a job. The commands a job runs about the host's
//! jobs ([`COMMANDS`]) go through its turn's jobs socket to the proj its
//! turn runs, on the host, so every engine sees and continues the same jobs,
//! sandboxed or not. A job with no socket says it can't reach them: never
//! its sandbox's own view of the jobs dir ("has no turns", "no session").
use crate::{config::Config, job::Job, scratch};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::{Duration, Instant},
};

/// What a job asks of the host's jobs; bare `proj` is `proj status`.
pub const COMMANDS: &[&str] = &["status", "state", "lifecycle", "log", "reply", "stop"];
/// How long one command may take on the host; the client waits a little
/// longer, so the host's own limit is what it hears.
const TIME: Duration = Duration::from_secs(300);
const MAX_REQUEST: u64 = 1 << 20;
const MAX_OUTPUT: usize = 16 << 20;

pub(crate) fn socket(cfg: &Config, job: &Job, n: &str) -> PathBuf {
    scratch::dir(cfg, job.id).join(format!("jobs-{n}.sock"))
}

/// Whether `args` is about the host's jobs.
pub fn about_jobs(args: &[String]) -> bool {
    args.first()
        .is_none_or(|a| a.is_empty() || COMMANDS.contains(&a.as_str()))
}

fn name(args: &[String]) -> &str {
    args.first()
        .map(String::as_str)
        .filter(|a| !a.is_empty())
        .unwrap_or("status")
}

/// What a sandbox with no way to the host's jobs says, for each command of
/// [`COMMANDS`].
pub fn unreachable(args: &[String], why: &str) -> String {
    format!(
        "proj {}: this job can't reach the host's jobs ({why}), so it can't see or continue them from here",
        name(args)
    )
}

#[derive(Serialize, Deserialize)]
struct Answer {
    code: u8,
    out: String,
    err: String,
}

/// `proj COMMAND...` inside a job: the host's answer, said as the host said
/// it, or a loud failure when the host can't be reached.
pub fn client(socket: Option<&Path>, args: &[String]) -> ExitCode {
    let answer = match socket {
        None => Err(unreachable(args, "its turn gave it no jobs socket")),
        Some(path) => exchange(path, args).map_err(|e| unreachable(args, &e)),
    };
    match answer {
        Ok(answer) => {
            let _ = std::io::stdout().lock().write_all(answer.out.as_bytes());
            let _ = std::io::stderr().lock().write_all(answer.err.as_bytes());
            ExitCode::from(answer.code)
        }
        Err(say) => {
            eprintln!("{say}");
            ExitCode::FAILURE
        }
    }
}

fn exchange(path: &Path, args: &[String]) -> Result<Answer, String> {
    let (_directory, address) = crate::probe::address(path)?;
    let mut stream = UnixStream::connect(address).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(TIME + Duration::from_secs(30)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(&serde_json::to_vec(args).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|e| e.to_string())?;
    let mut answer = Vec::new();
    stream
        .take(2 * MAX_OUTPUT as u64 + 4096)
        .read_to_end(&mut answer)
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&answer).map_err(|e| format!("the host's answer: {e}"))
}

/// One request on the host: run as the proj the turn runs, outside the
/// job's environment, so it is the host's own answer.
pub(crate) fn serve(cfg: &Config, mut stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let mut request = Vec::new();
    if (&mut stream)
        .take(MAX_REQUEST)
        .read_to_end(&mut request)
        .is_err()
    {
        return;
    }
    let answer = answer(cfg, &request).unwrap_or_else(|err| Answer {
        code: 1,
        out: String::new(),
        err: format!("{err}\n"),
    });
    let _ = stream.write_all(&serde_json::to_vec(&answer).expect("jobs answer"));
}

fn answer(cfg: &Config, request: &[u8]) -> Result<Answer, String> {
    let args: Vec<String> = serde_json::from_slice(request).map_err(|e| e.to_string())?;
    if !about_jobs(&args) {
        return Err(format!(
            "proj {}: only {} reach the host's jobs",
            name(&args),
            COMMANDS.join(", ")
        ));
    }
    let mut command = Command::new(&cfg.me);
    command
        .args(&args)
        .env_remove("PROJ_JOB")
        .env_remove("PROJ_JOBS_SOCKET")
        .env_remove("PROJ_PROBE_SOCKET");
    let out = burrow_process::output(&mut command, None, Instant::now() + TIME, MAX_OUTPUT)
        .map_err(|e| format!("proj {}: {e}", name(&args)))?;
    Ok(Answer {
        code: out
            .status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(1),
        out: String::from_utf8_lossy(&out.stdout).into_owned(),
        err: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &[&str]) -> Vec<String> {
        s.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn only_commands_about_jobs_cross() {
        assert!(about_jobs(&[]));
        for c in COMMANDS {
            assert!(about_jobs(&words(&[c, "x"])));
        }
        for c in ["run", "await", "ask", "done", "probe", "_codex", "engines"] {
            assert!(!about_jobs(&words(&[c])), "{c}");
        }
    }

    #[test]
    fn the_host_answers_with_its_own_output_and_exit_status() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg: Config = serde_json::from_value(crate::config::tests::example()).unwrap();
        cfg.scratch = dir.path().join("scratch");
        // A host proj that says what it was asked and what of the job's
        // environment it was given.
        cfg.me = dir.path().join("proj");
        burrow_test::executable(
            &cfg.me,
            "#!/bin/sh\necho \"host: $*\" \"job=${PROJ_JOB-unset}\" \"socket=${PROJ_JOBS_SOCKET-unset}\"\necho said >&2\nexit 3\n",
        );
        let job = Job::new(&cfg, "asking");
        let _bridge = crate::probe::Bridge::start(&cfg, &job, "1", None).unwrap();
        let path = socket(&cfg, &job, "1");
        let got = exchange(&path, &words(&["reply", "other", "carry on"])).unwrap();
        assert_eq!(
            got.out,
            "host: reply other carry on job=unset socket=unset\n"
        );
        assert_eq!(got.err, "said\n");
        assert_eq!(got.code, 3);
        // Anything else is refused on the host, whatever the client sends.
        let refused = exchange(&path, &words(&["run", "web", "go"])).unwrap();
        assert_eq!(refused.code, 1);
        assert!(refused.err.contains("only status"), "{}", refused.err);
        assert!(refused.out.is_empty());
    }

    #[test]
    fn a_job_without_the_socket_says_it_cant_reach_the_hosts_jobs() {
        let said = unreachable(
            &words(&["reply", "x", "go"]),
            "its turn gave it no jobs socket",
        );
        assert!(said.starts_with("proj reply: this job can't reach the host's jobs"));
        assert_eq!(client(None, &words(&["log", "x"])), ExitCode::FAILURE);
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("jobs-1.sock");
        assert!(exchange(&gone, &words(&["status"])).is_err());
    }
}
