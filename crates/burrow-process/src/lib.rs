//! Bounded child transport and confined process-state reads.
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
    if Instant::now() >= until {
        return Err("command exceeded its time limit".into());
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
        .map_err(|e| e.to_string())?;
    let pid = Pid::from_child(&child);
    let result = (|| {
        let mut stdin = child.stdin.take();
        if let Some(pipe) = &stdin {
            nonblocking(pipe)?;
        }
        let mut input = input.unwrap_or_default();
        let mut stdout = child.stdout.take().ok_or("missing stdout")?;
        let mut stderr = child.stderr.take().ok_or("missing stderr")?;
        nonblocking(&stdout)?;
        nonblocking(&stderr)?;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let (mut out_done, mut err_done) = (false, false);
        loop {
            if Instant::now() >= until {
                return Err("command exceeded its time limit".into());
            }
            if let Some(pipe) = &mut stdin {
                if !input.is_empty() {
                    match pipe.write(input) {
                        Ok(n) => input = &input[n..],
                        Err(e)
                            if matches!(
                                e.kind(),
                                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                            ) => {}
                        Err(e) => return Err(e.to_string()),
                    }
                }
                if input.is_empty() {
                    stdin = None;
                }
            }
            if !out_done {
                out_done = read(&mut stdout, &mut out, max)?;
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
                    stdout: out,
                    stderr: err,
                });
            }
            sleep(Duration::from_millis(5).min(until.saturating_duration_since(Instant::now())));
        }
    })();
    let _ = kill_process_group(pid, Signal::KILL);
    let _ = child.wait();
    result
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
