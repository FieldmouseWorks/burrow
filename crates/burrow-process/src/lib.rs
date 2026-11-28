//! Bounded child transport, fences and confined process-state reads.
pub mod fence;
pub mod lifecycle;
pub mod state_reads;
use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    process::{Pid, Signal, kill_process_group},
};
use std::{
    io::{self, Read, Write},
    os::unix::process::CommandExt,
    process::{Command, Output, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

/// Whether path names a program a child can be started from: a file,
/// links followed, with an execute bit.
pub fn executable(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
        Ok(())
    } else {
        Err("not an executable file".into())
    }
}

fn nonblocking(fd: &impl std::os::fd::AsFd) -> Result<(), String> {
    let flags = fcntl_getfl(fd).map_err(|e| e.to_string())?;
    fcntl_setfl(fd, flags | OFlags::NONBLOCK).map_err(|e| e.to_string())
}

fn read(pipe: &mut impl Read, bytes: &mut Vec<u8>, max: usize) -> Result<bool, String> {
    let mut buffer = [0; 8192];
    match pipe.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(n) => {
            if bytes.len() + n > max {
                return Err(format!("command output exceeded {max} bytes"));
            }
            bytes.extend_from_slice(&buffer[..n]);
            Ok(false)
        }
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e.to_string()),
    }
}

/// Feed input and drain both pipes without blocking past `until`. Cap each
/// output, and kill and reap the process group on every exit, even if a
/// descendant holds a pipe open after the command itself has finished.
pub fn output(
    command: &mut Command,
    input: Option<&[u8]>,
    until: Instant,
    max: usize,
) -> Result<Output, String> {
    output_unless(command, input, until, max, &|| false)
}

/// Partial output returned when [`stream_unless`] fails (for example on
/// deadline or stop), preserving whatever bytes were already read.
#[derive(Debug)]
pub struct OutputError {
    pub reason: String,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// [`output`], stopped the same way, process group and all, as soon as
/// `stop` says so: the work it was for was abandoned.
pub fn output_unless(
    command: &mut Command,
    input: Option<&[u8]>,
    until: Instant,
    max: usize,
    stop: &dyn Fn() -> bool,
) -> Result<Output, String> {
    stream_unless(command, input, Some(until), max, stop, &mut |_| {}).map_err(|e| e.reason)
}

fn flush_stdout_lines(out: &[u8], emitted: &mut usize, on_stdout_line: &mut dyn FnMut(&str)) {
    while let Some(rel) = out[*emitted..].iter().position(|&b| b == b'\n') {
        let end = *emitted + rel;
        let line_bytes = out[*emitted..end]
            .strip_suffix(b"\r")
            .unwrap_or(&out[*emitted..end]);
        let line = String::from_utf8_lossy(line_bytes);
        on_stdout_line(&line);
        *emitted = end + 1;
    }
}

/// Like [`output_unless`], but with an optional deadline (`None` means no
/// internal time limit), streaming complete `\n`-terminated stdout lines via
/// `on_stdout_line` as they arrive, and returning any partial stdout/stderr in
/// [`OutputError`] if the run is stopped or times out.
pub fn stream_unless(
    command: &mut Command,
    input: Option<&[u8]>,
    until: Option<Instant>,
    max: usize,
    stop: &dyn Fn() -> bool,
    on_stdout_line: &mut dyn FnMut(&str),
) -> Result<Output, OutputError> {
    if until.is_some_and(|u| Instant::now() >= u) {
        return Err(OutputError {
            reason: "command exceeded its time limit".into(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        });
    }
    let mut child = command
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| OutputError {
            reason: e.to_string(),
            stdout: Vec::new(),
            stderr: Vec::new(),
        })?;
    let pid = Pid::from_child(&child);
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = (|| -> Result<Output, String> {
        let mut stdin = child.stdin.take();
        if let Some(pipe) = &stdin {
            nonblocking(pipe)?;
        }
        let mut input = input.unwrap_or_default();
        let mut stdout = child.stdout.take().ok_or("missing stdout")?;
        let mut stderr = child.stderr.take().ok_or("missing stderr")?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let (mut out_done, mut err_done) = (false, false);
        let mut emitted = 0usize;
        loop {
            if until.is_some_and(|u| Instant::now() >= u) {
                return Err("command exceeded its time limit".into());
            }
            if stop() {
                return Err("command was stopped".into());
            }
            if let Some(pipe) = &mut stdin {
                let mut done = input.is_empty();
                if !done {
                    match pipe.write(input) {
                        Ok(n) => {
                            input = &input[n..];
                            done = input.is_empty();
                        }
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => {
                            done = true;
                        }
                        Err(e) => return Err(e.to_string()),
                    }
                }
                if done {
                    stdin = None;
                }
            }
            if !out_done {
                out_done = read(&mut stdout, &mut out, max)?;
                flush_stdout_lines(&out, &mut emitted, on_stdout_line);
            }
            if !err_done {
                err_done = read(&mut stderr, &mut err, max)?;
            }
            if out_done
                && err_done
                && let Some(status) = child.try_wait().map_err(|e| e.to_string())?
            {
                return Ok(Output {
                    status,
                    stdout: std::mem::take(&mut out),
                    stderr: std::mem::take(&mut err),
                });
            }
            let wait = match until {
                Some(u) => {
                    Duration::from_millis(5).min(u.saturating_duration_since(Instant::now()))
                }
                None => Duration::from_millis(5),
            };
            sleep(wait);
        }
    })();
    let _ = kill_process_group(pid, Signal::KILL);
    let _ = child.wait();
    result.map_err(|reason| OutputError {
        reason,
        stdout: out,
        stderr: err,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_input_both_outputs_and_exit_status() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "cat; printf error >&2; exit 7"]);
        let out = output(
            &mut command,
            Some(b"input"),
            Instant::now() + Duration::from_secs(2),
            1024,
        )
        .unwrap();
        assert_eq!(out.stdout, b"input");
        assert_eq!(out.stderr, b"error");
        assert_eq!(out.status.code(), Some(7));
    }

    #[test]
    fn a_child_that_does_not_read_cannot_hold_stdin_past_the_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec sleep 60"]);
        let start = Instant::now();
        let input = vec![b'x'; 1024 * 1024];
        let error = output(
            &mut command,
            Some(&input),
            start + Duration::from_millis(60),
            1024,
        )
        .unwrap_err();
        assert!(error.contains("time limit"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_stop_kills_the_whole_group_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let pid = dir.path().join("pid");
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            &format!("sleep 30 & echo $! > {}; wait", pid.display()),
        ]);
        let started = Instant::now();
        // Stopped once the pid is written whole, not when the redirect
        // makes the file.
        let stop = || std::fs::read_to_string(&pid).is_ok_and(|p| p.ends_with('\n'));
        let out = output_unless(
            &mut command,
            None,
            Instant::now() + Duration::from_secs(60),
            1024,
            &stop,
        );
        assert_eq!(out.unwrap_err(), "command was stopped");
        assert!(started.elapsed() < Duration::from_secs(20));
        let child = std::fs::read_to_string(&pid).unwrap();
        // The group's sleep was killed with it; give its parent's reaping a
        // moment, since init reaps an orphan.
        let gone = (0..200).any(|_| {
            sleep(Duration::from_millis(10));
            !std::path::Path::new(&format!("/proc/{}", child.trim())).exists()
        });
        assert!(gone, "the group's other process is gone");
    }

    #[test]
    fn child_exiting_before_reading_stdin_still_returns_its_output() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf 'out'; printf 'err' >&2; exit 1"]);
        let input = vec![b'x'; 256 * 1024];
        let out = output(
            &mut command,
            Some(&input),
            Instant::now() + Duration::from_secs(2),
            1024,
        )
        .unwrap();
        assert_eq!(out.stdout, b"out");
        assert_eq!(out.stderr, b"err");
        assert_eq!(out.status.code(), Some(1));
    }

    #[test]
    fn bounds_each_output_pipe() {
        for script in ["printf 12345", "printf 12345 >&2"] {
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            assert!(
                output(
                    &mut command,
                    None,
                    Instant::now() + Duration::from_secs(2),
                    4
                )
                .unwrap_err()
                .contains("exceeded 4 bytes")
            );
        }
    }
}
