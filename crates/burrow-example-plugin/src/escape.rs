//! What it wasn't granted, tried with all the VM lets it do, root included.

use crate::{Look, Report, uses};
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::process::{Command, Stdio};
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(5);

/// `args` as root in the VM with `input` on stdin: its output, when it
/// succeeded.
pub(crate) fn root(args: &[&str], input: &str) -> Option<String> {
    let mut child = Command::new("sudo")
        .arg("-n")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child.stdin.take()?.write_all(input.as_bytes()).ok()?;
    let output = child.wait_with_output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What a CONNECT to `host`'s 443 through the proxy's address got.
pub(crate) fn through_proxy(host: &str) -> String {
    let got = || -> std::io::Result<String> {
        let proxy = SocketAddr::from(([10, 0, 2, 100], 3128));
        let mut proxy = TcpStream::connect_timeout(&proxy, PATIENCE)?;
        proxy.set_read_timeout(Some(PATIENCE))?;
        write!(
            proxy,
            "CONNECT {host}:443 HTTP/1.1\r\nHost: {host}:443\r\n\r\n"
        )?;
        let mut answer = Vec::new();
        let mut part = [0; 4096];
        while let Ok(n @ 1..) = proxy.read(&mut part) {
            answer.extend_from_slice(&part[..n]);
        }
        Ok(String::from_utf8_lossy(&answer).into_owned())
    };
    got().unwrap_or_else(|e| format!("no proxy: {e}"))
}

pub(crate) fn every(look: &Look, report: &mut Report) {
    eprintln!("example plugin: asking the proxy");
    for host in &look.hosts {
        report.proxied.insert(host.clone(), through_proxy(host));
    }
    eprintln!("example plugin: dialing around it");
    for at in &look.tcp {
        if at
            .parse()
            .is_ok_and(|to| TcpStream::connect_timeout(&to, PATIENCE).is_ok())
        {
            report.dialed.push(format!("tcp:{at}"));
        }
    }
    for at in &look.udp {
        let echoed = || -> std::io::Result<usize> {
            let socket = UdpSocket::bind("0.0.0.0:0")?;
            socket.set_read_timeout(Some(PATIENCE))?;
            socket.connect(at.as_str())?;
            socket.send(b"anyone\n")?;
            socket.recv(&mut [0; 512])
        };
        if echoed().is_ok() {
            report.dialed.push(format!("udp:{at}"));
        }
    }
    for port in &look.vsock {
        let dial = format!("VSOCK-CONNECT:2:{port}");
        let socat = ["timeout", "5", "socat", "-u", dial.as_str(), "STDOUT"];
        if root(&socat, "").is_some_and(|said| !said.is_empty()) {
            report.dialed.push(format!("vsock:{port}"));
        }
    }
    eprintln!("example plugin: searching");
    report.found = found(&look.secrets);
    eprintln!("example plugin: searched");
    for path in &look.read {
        if root(&["test", "-e", &path.to_string_lossy()], "").is_some() {
            report.readable.push(path.clone());
        }
    }
    for path in &look.write {
        let at = path.to_string_lossy();
        // A read-only mount is the first thing root would undo.
        let script = "mount -o remount,rw /nix/store; mount -o remount,rw /nix/.ro-store; \
                      mkdir -p \"$(dirname \"$1\")\" && echo plugin >\"$1\"";
        if root(&["sh", "-c", script, "sh", &at], "").is_some() {
            report.written.push(path.clone());
        }
    }
    eprintln!("example plugin: mounting");
    for tag in &look.tags {
        let script = "d=$(mktemp -d) && mount -t virtiofs \"$1\" \"$d\"";
        if root(&["sh", "-c", script, "sh", tag], "").is_some() {
            report.mounted.push(tag.clone());
        }
    }
    for words in &look.asks {
        report
            .asked
            .push(uses::host_jobs(words).unwrap_or_else(|why| why));
    }
}

/// Where a secret could be in the VM, searched a place at a time. Of
/// fw_cfg it's `opt/`, all of it that a host puts there: the kernel reads
/// the rest, the firmware's tables, a byte a VM exit, for minutes.
const PLACES: &[&str] = &[
    "/home",
    "/root",
    "/tmp",
    "/data",
    "/etc",
    "/var",
    "/run",
    "/sys/firmware/qemu_fw_cfg/by_name/opt",
    "environ",
];

/// Which of `needles` are anywhere in the VM, and any place whose search
/// didn't end, which is never "nothing there". They reach the search on
/// stdin, so no command line or journal of it holds one.
fn found(needles: &[String]) -> Vec<String> {
    if needles.is_empty() {
        return Vec::new();
    }
    let script = "patterns=$(cat); \
        if [ \"$1\" = environ ]; then \
          cat /proc/[0-9]*/environ | timeout 120 grep -oaF -e \"$patterns\"; \
        elif [ -e \"$1\" ]; then \
          case \"$1\" in /sys/*) r=-R ;; *) r=-r ;; esac; \
          timeout 120 grep $r -hoaF -D skip -e \"$patterns\" \"$1\"; \
        else exit 0; fi 2>/dev/null; \
        [ $? -ne 124 ] || echo unfinished";
    let patterns = needles.join("\n");
    let mut found = Vec::new();
    for place in PLACES {
        eprintln!("example plugin: searching {place}");
        let said = root(&["sh", "-c", script, "sh", place], &patterns);
        let said = said.unwrap_or_else(|| "unfinished".to_owned());
        for line in said.lines() {
            if line == "unfinished" {
                found.push(format!("unfinished: {place}"));
            } else if needles.iter().any(|needle| needle == line) {
                found.push(line.to_owned());
            }
        }
    }
    found.sort();
    found.dedup();
    found
}
