//! Fixtures shared by the workspace's tests.

use std::ffi::OsStr;
use std::fs;
use std::io::{BufRead, BufReader, Read, Seek, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Install an executable without ever opening it for writing in this process.
///
/// A concurrent fork can inherit a writer until it execs, making another
/// execution fail with ETXTBSY. Only the install child opens the destination;
/// the parent's temporary source is never executed. Wait for installation
/// before returning so callers can immediately run the result.
pub fn executable(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    let mut source = tempfile::NamedTempFile::new().unwrap();
    source.write_all(contents.as_ref()).unwrap();
    let output = Command::new("install")
        .args(["-m", "0755", "--"])
        .arg(source.path())
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A command for a program under test that starts from the environment CI
/// gives it: every `PROJ_*` variable the test inherited is removed.
///
/// A proj job runs with its own `PROJ_JOB`, `PROJ_CONFIG`, sockets and the
/// rest, and proj's programs act on those first: a child that inherits them
/// talks to the job's host and config instead of the test's fixture. Spawn
/// proj's programs, and test binaries re-running themselves, through this, and
/// set the `PROJ_*` variables the test means on the returned command.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"PROJ_") {
            command.env_remove(key);
        }
    }
    command
}

/// What a command run to a deadline said, and whether it was killed there.
pub struct Within {
    pub output: Output,
    /// It ran past the deadline, so it and everything it started were
    /// killed.
    pub killed: bool,
}

/// Run a command to its end or to `deadline`, whichever comes first, its
/// output in files: at the deadline it and everything it started (in
/// process groups of their own too, as a request with a deadline is) are
/// killed, and nothing left holds its output open. A test of code that
/// should stop waiting then fails in seconds on code that waits forever,
/// rather than hold its build until Nix's timeout.
pub fn output_within(command: &mut Command, deadline: Duration) -> Within {
    let mut out = tempfile::tempfile().unwrap();
    let mut err = tempfile::tempfile().unwrap();
    let mut child = command
        .stdin(Stdio::null())
        .stdout(out.try_clone().unwrap())
        .stderr(err.try_clone().unwrap())
        .spawn()
        .unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let killed = status.is_none();
    let status = status.unwrap_or_else(|| {
        // Not yet reaped, so its pid and its descendants' parent links hold.
        end_tree(i32::try_from(child.id()).unwrap());
        child.wait().unwrap()
    });
    let read = |f: &mut fs::File| {
        let mut v = Vec::new();
        f.rewind().unwrap();
        f.read_to_end(&mut v).unwrap();
        v
    };
    Within {
        output: Output {
            status,
            stdout: read(&mut out),
            stderr: read(&mut err),
        },
        killed,
    }
}

/// A process and every process descended from it, from the kernel's
/// parent links.
fn tree(root: i32) -> Vec<i32> {
    let parents: Vec<(i32, i32)> = fs::read_dir("/proc")
        .map(|dir| {
            dir.filter_map(|e| {
                let pid: i32 = e.ok()?.file_name().to_str()?.parse().ok()?;
                let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
                // After the name: state, then parent.
                let parent = stat.rsplit_once(") ")?.1.split_whitespace().nth(1)?;
                Some((pid, parent.parse().ok()?))
            })
            .collect()
        })
        .unwrap_or_default();
    let mut found = vec![root];
    let mut i = 0;
    while i < found.len() {
        let parent = found[i];
        found.extend(
            parents
                .iter()
                .filter(|(_, p)| *p == parent)
                .map(|(c, _)| *c),
        );
        i += 1;
    }
    found
}

/// Stop a process and its descendants, until no new one turns up, so none
/// starts another or leaves the tree as its parent dies; then kill them.
fn end_tree(root: i32) {
    use rustix::process::{Pid, Signal, kill_process};
    let signal = |pid: i32, s: Signal| {
        if let Some(pid) = Pid::from_raw(pid) {
            let _ = kill_process(pid, s);
        }
    };
    let started = Instant::now();
    let mut stopped: Vec<i32> = Vec::new();
    loop {
        let new: Vec<i32> = tree(root)
            .into_iter()
            .filter(|p| !stopped.contains(p))
            .collect();
        if new.is_empty() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "{new:?}, started by {root}, won't stop"
        );
        for pid in new {
            signal(pid, Signal::STOP);
            stopped.push(pid);
        }
    }
    for pid in stopped {
        signal(pid, Signal::KILL);
    }
}

/// Change fixture permissions, never making a regular file executable.
/// Directories need search bits; executable files must use [`executable`].
pub fn permissions(path: impl AsRef<Path>, mode: u32) {
    let path = path.as_ref();
    assert!(
        fs::metadata(path).unwrap().is_dir() || mode & 0o111 == 0,
        "use burrow_test::executable for {}",
        path.display()
    );
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Copy a data fixture, refusing executable sources or destinations.
/// `fs::copy` also copies execute bits, so executable copies must go through
/// [`executable`] with the source's bytes instead.
pub fn copy_file(from: impl AsRef<Path>, to: impl AsRef<Path>) {
    let (from, to) = (from.as_ref(), to.as_ref());
    for path in [from, to] {
        if let Ok(meta) = fs::metadata(path) {
            assert!(
                meta.permissions().mode() & 0o111 == 0,
                "use burrow_test::executable for {}",
                path.display()
            );
        }
    }
    fs::copy(from, to).unwrap();
}

/// A directory served over HTTP under `/builds`, as nginx serves the
/// machine's build store: a file it holds, or a 404. Returns its URL; it
/// serves until the test process ends.
pub fn build_store(dir: impl AsRef<Path>) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/builds", l.local_addr().unwrap());
    let dir = dir.as_ref().to_path_buf();
    std::thread::spawn(move || {
        for c in l.incoming() {
            let Ok(mut c) = c else { continue };
            let Ok(read) = c.try_clone() else { continue };
            let mut r = BufReader::new(read);
            let mut line = String::new();
            let _ = r.read_line(&mut line);
            let mut w = line.split_whitespace();
            let (method, target) = (w.next().unwrap_or(""), w.next().unwrap_or(""));
            let mut h = String::new();
            while r.read_line(&mut h).is_ok_and(|n| n > 0) && h != "\r\n" {
                h.clear();
            }
            let file = target
                .strip_prefix("/builds/")
                .filter(|f| !f.split('/').any(|p| p == ".." || p.is_empty()))
                .and_then(|f| fs::read(dir.join(f)).ok());
            let (status, body) = match file {
                Some(b) => ("200 OK", b),
                None => ("404 Not Found", Vec::new()),
            };
            let _ = write!(
                c,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            if method != "HEAD" {
                let _ = c.write_all(&body);
            }
        }
    });
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_install_and_execution() {
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    let dir = tempfile::tempdir().unwrap();
                    let file = dir.path().join("stand-in");
                    for _ in 0..100 {
                        executable(&file, "#!/bin/sh\nprintf installed");
                        assert_eq!(
                            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                            0o755
                        );
                        let out = Command::new(&file).output().unwrap();
                        assert!(out.status.success());
                        assert_eq!(out.stdout, b"installed");
                    }
                });
            }
        });
    }

    /// A command that never ends is killed at the deadline, with what it
    /// started in a process group of its own; one that ends says what it
    /// said.
    #[test]
    fn a_command_that_never_ends_is_killed_at_its_deadline() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let started = Instant::now();
        let ran = output_within(
            Command::new("sh").args([
                "-c",
                "set -m; echo said; sleep infinity & echo $! > \"$0\"; wait",
                pid.to_str().unwrap(),
            ]),
            Duration::from_millis(500),
        );
        assert!(ran.killed);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(!ran.output.status.success());
        assert_eq!(ran.output.stdout, b"said\n");
        let pid = fs::read_to_string(&pid).unwrap();
        let gone = Instant::now();
        // Gone, or only waiting to be reaped.
        while fs::read_to_string(format!("/proc/{}/stat", pid.trim()))
            .is_ok_and(|s| !s.contains(") Z "))
        {
            assert!(gone.elapsed() < Duration::from_secs(5), "{pid} still runs");
            std::thread::sleep(Duration::from_millis(10));
        }
        let ran = output_within(
            Command::new("sh").args(["-c", "echo done; exit 3"]),
            Duration::from_secs(60),
        );
        assert!(!ran.killed);
        assert_eq!(ran.output.status.code(), Some(3));
        assert_eq!(ran.output.stdout, b"done\n");
    }

    #[test]
    #[should_panic(expected = "use burrow_test::executable")]
    fn permissions_cannot_make_a_file_executable() {
        let file = tempfile::NamedTempFile::new().unwrap();
        permissions(file.path(), 0o755);
    }
    #[test]
    #[should_panic(expected = "use burrow_test::executable")]
    fn copying_cannot_install_an_executable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("executable");
        executable(&source, "#!/bin/sh\nexit 0\n");
        copy_file(source, dir.path().join("copy"));
    }

    /// The child the parent starts with a job's environment; it checks what a
    /// command built by [`command`] would see.
    #[test]
    fn command_child() {
        if std::env::var_os("PROJ_JOB").is_none() {
            return;
        }
        let out = command("env")
            .env("PROJ_CONFIG", "fixture")
            .output()
            .unwrap();
        let env = String::from_utf8(out.stdout).unwrap();
        let proj: Vec<_> = env.lines().filter(|l| l.starts_with("PROJ_")).collect();
        assert_eq!(proj, ["PROJ_CONFIG=fixture"]);
        println!("checked");
    }

    #[test]
    fn command_drops_the_inherited_job_environment() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::command_child", "--nocapture"])
            .env("PROJ_JOB", "job-1")
            .env("PROJ_CONFIG", "/job/config.json")
            .env("PROJ_HOST_JOBS_SOCKET", "/job/host.sock")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("checked"));
    }
}
