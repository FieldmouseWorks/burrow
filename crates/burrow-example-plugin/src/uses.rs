//! Each capability, used as its grant renders it.

use crate::{Look, Report, escape};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::Duration;

const PATIENCE: Duration = Duration::from_secs(60);

pub(crate) fn every(look: &Look, report: &mut Report) {
    let mut tried = |name: &str, outcome: Result<String, String>| {
        eprintln!("example plugin: tried {name}");
        report.works.insert(name.to_owned(), outcome.is_ok());
        report
            .said
            .insert(name.to_owned(), outcome.unwrap_or_else(|why| why));
    };
    tried("workspace", workspace());
    tried("git-metadata", git_metadata());
    tried(
        "job-state",
        host_jobs(&["list".to_owned()]).and_then(answered),
    );
    tried("probe", exchange("probe", b"memory").and_then(answered));
    tried(
        "units-clear",
        exchange("units-clear", br#"["/nix/store/example/bin/crashed"]"#).and_then(answered),
    );
    tried("heavy", run("heavy", &["--", "printenv", "HEAVY_JOBS"]));
    tried("system", run("rg", &["--version"]));
    tried("toolchains", run("gcc", &["--version"]));
    tried("nix", nix());
    tried("kvm", kvm());
    tried("mcp", exchange("mcp-0", b"ping\n").and_then(nonempty));
    tried("network", network(look));
}

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if output.status.success() {
        nonempty(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

fn nonempty(text: String) -> Result<String, String> {
    if text.trim().is_empty() {
        Err("no answer".to_owned())
    } else {
        Ok(text)
    }
}

/// A service of proj's answers `{"Ok": ...}`.
fn answered(text: String) -> Result<String, String> {
    if text.starts_with(r#"{"Ok""#) {
        Ok(text)
    } else {
        Err(text)
    }
}

fn workspace() -> Result<String, String> {
    let was = std::fs::read_to_string("a").map_err(|e| format!("a: {e}"))?;
    std::fs::write("plugin-was-here", "the plugin's\n").map_err(|e| format!("writing: {e}"))?;
    Ok(was)
}

fn git_metadata() -> Result<String, String> {
    let head = std::fs::read_to_string(".git/HEAD").map_err(|e| format!(".git/HEAD: {e}"))?;
    nonempty(head)
}

/// Asks a service at the socket its grant would have, told of it or not.
fn exchange(service: &str, request: &[u8]) -> Result<String, String> {
    let at = format!("/run/agent/{service}.sock");
    let fail = |e: std::io::Error| format!("{at}: {e}");
    let mut socket = UnixStream::connect(&at).map_err(fail)?;
    socket.set_read_timeout(Some(PATIENCE)).map_err(fail)?;
    socket.write_all(request).map_err(fail)?;
    socket.shutdown(Shutdown::Write).map_err(fail)?;
    let mut answer = String::new();
    socket.read_to_string(&mut answer).map_err(fail)?;
    Ok(answer)
}

/// What proj's jobs socket says to `words`.
pub(crate) fn host_jobs(words: &[String]) -> Result<String, String> {
    let request = serde_json::to_vec(words).map_err(|e| e.to_string())?;
    exchange("host-jobs", &request)
}

fn nix() -> Result<String, String> {
    run(
        "nix",
        &[
            "eval",
            "--impure",
            "--raw",
            "--expr",
            r#"builtins.toFile "example" "the plugin's""#,
        ],
    )
}

/// The device, as the user and as root.
fn kvm() -> Result<String, String> {
    if std::fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/kvm")
        .is_ok()
    {
        return Ok("opened".to_owned());
    }
    escape::root(
        &[
            "sh",
            "-c",
            "test -c /dev/kvm && exec 3</dev/kvm && echo opened as root",
        ],
        "",
    )
    .ok_or_else(|| "/dev/kvm doesn't open".to_owned())
}

fn network(look: &Look) -> Result<String, String> {
    let (host, expected) = look.allowed.as_ref().ok_or("no allowed host to try")?;
    let said = escape::through_proxy(host);
    if said.contains(expected) {
        Ok(said)
    } else {
        Err(said)
    }
}
