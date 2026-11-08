//! Bounded child output, shared by guest transport and host GitHub requests.
use rustix::{
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
    process::{Pid, Signal, kill_process_group},
};
use std::{
    io::{self, Read},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    thread::sleep,
    time::{Duration, Instant},
};

/// Poll a nonblocking pipe: neither an SSH login profile nor a descendant
/// holding stdout open can hide the deadline. Only a fixed buffer is read,
/// and stdout is capped before appending. Kill the local process group on
/// every exit, including successful reads that left descendants behind.
pub(crate) fn output(
    command: &mut Command,
    until: Instant,
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    if Instant::now() >= until {
        return Err("command exceeded its time limit".into());
    }
    let mut child = command
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let pid = Pid::from_child(&child);
    let result = (|| {
        let mut stdout = child.stdout.take().ok_or("missing command stdout")?;
        let flags = fcntl_getfl(&stdout).map_err(|e| e.to_string())?;
        fcntl_setfl(&stdout, flags | OFlags::NONBLOCK).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 8192];
        let mut eof = false;
        loop {
            if Instant::now() >= until {
                return Err("command exceeded its time limit".into());
            }
            if !eof {
                match stdout.read(&mut buffer) {
                    Ok(0) => eof = true,
                    Ok(n) => {
                        if bytes.len() + n > max_bytes {
                            return Err(format!("command output exceeded {max_bytes} bytes"));
                        }
                        bytes.extend_from_slice(&buffer[..n]);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => return Err(e.to_string()),
                }
            }
            if eof && let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                return if status.success() {
                    Ok(bytes)
                } else {
                    Err(format!("command failed: {status}"))
                };
            }
            sleep(Duration::from_millis(5).min(until.saturating_duration_since(Instant::now())));
        }
    })();
    let _ = kill_process_group(pid, Signal::KILL);
    let _ = child.wait();
    result
}
