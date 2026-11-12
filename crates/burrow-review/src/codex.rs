//! Codex's look: the codex CLI, read-only in a sandbox (bubblewrap) that
//! shows it its snapshot and its PR's session and little else, with no
//! network but its own API through the gate (egress.rs).

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::fs::symlink;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use burrow_codex::{logged_out, spent, wait_for};

use crate::egress::Gate;
use crate::scrub::tail;
use crate::{Failed, Service, now};

/// Where a look sees its snapshot: the same path for every head, so a
/// session resumed on a PR's new head finds the checkout where it left it.
pub const CHECKOUT: &str = "/review";

/// Where Codex's API is: it goes through the gate as its HTTPS proxy, to
/// these hosts only.
pub const HOSTS: &[&str] = &["chatgpt.com", "auth.openai.com", "api.openai.com"];

/// The answer it's held to, which the steward posts (burrow-upkeep's
/// post.rs).
pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "findings": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "line": {"type": "integer"},
                    "severity": {"type": "string", "enum": ["P0", "P1", "P2"]},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "covers": {"type": "string"},
                },
                "required": ["path", "line", "severity", "title", "body", "covers"],
                "additionalProperties": false,
            }},
            "summary": {"type": "string"},
        },
        "required": ["findings", "summary"],
        "additionalProperties": false,
    })
}

/// argv in a sandbox of its own: it sees the store paths it runs on (the
/// agents, the tools and their closure, `box.store`), the machine's CA
/// bundle, the links `box.links` makes, /etc's passwd and group, its
/// snapshot (read-only, at CHECKOUT), its PR's session (`home`), its temp
/// dir `t`, `mounts` (each a bwrap flag and a path, the same path inside),
/// and nothing else of the machine.
///
/// The service's user can read every queued head's snapshot, every answer
/// and every PR's session, and an agent talked into reading another repo's
/// would post it in this one's review. Nor does it see the rest of the
/// store: every flake this machine has evaluated has its sources there,
/// private repos' included, and /etc's files link into the machine's
/// configuration.
///
/// It has no network: argv runs behind the bridge to the gate on
/// `t`/egress.sock, which the caller opens, with `names` in its environment
/// pointing at the bridge. The snapshot is the steward's, so git needs
/// telling it's safe to read: a git config of its own, for that one path.
pub fn sandboxed(
    s: &Service,
    snap: &Path,
    home: &Path,
    t: &Path,
    names: &[&str],
    argv: &[OsString],
    mounts: &[(&str, PathBuf)],
) -> io::Result<Vec<OsString>> {
    fs::create_dir_all(home)?;
    fs::write(
        t.join("gitconfig"),
        format!("[safe]\n\tdirectory = {CHECKOUT}\n"),
    )?;
    let b = &s.cfg.sandboxed;
    let store = fs::read_to_string(&b.store)?;
    let mut a: Vec<OsString> = Vec::new();
    let mut add =
        |xs: &[&dyn AsRef<std::ffi::OsStr>]| a.extend(xs.iter().map(|x| x.as_ref().to_owned()));
    add(&[&"--unshare-all", &"--die-with-parent", &"--new-session"]);
    add(&[&"--proc", &"/proc", &"--dev", &"/dev", &"--tmpfs", &"/tmp"]);
    for p in store.split_whitespace() {
        add(&[&"--ro-bind", &p, &p]);
    }
    for p in ["/etc/passwd", "/etc/group"] {
        add(&[&"--ro-bind-try", &p, &p]);
    }
    add(&[
        &"--ro-bind",
        &b.ca_bundle,
        &"/etc/ssl/certs/ca-certificates.crt",
    ]);
    for (at, to) in &b.links {
        add(&[&"--symlink", to, at]);
    }
    add(&[
        &"--ro-bind",
        &snap,
        &CHECKOUT,
        &"--bind",
        &home,
        &home,
        &"--bind",
        &t,
        &t,
    ]);
    for (flag, p) in mounts {
        add(&[flag, p, p]);
    }
    add(&[
        &"--ro-bind",
        &t.join("gitconfig"),
        &s.home.join(".gitconfig"),
    ]);
    add(&[
        &"--chdir",
        &CHECKOUT,
        &"--",
        &s.exe,
        &"bridge",
        &t.join("egress.sock"),
        &names.join(","),
        &"--",
    ]);
    a.extend(argv.iter().cloned());
    Ok(a)
}

/// What a command printed, and how it exited; None if it was still at it
/// when its time ran out, and was stopped.
enum Ran {
    Ended(i32, String, String),
    TimedOut,
    Paused(String),
}

fn run(
    mut cmd: Command,
    input: &str,
    timeout: Duration,
    paused: impl Fn() -> bool,
) -> io::Result<Ran> {
    if paused() {
        return Ok(Ran::Paused(String::new()));
    }
    let mut child = cmd
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (mut stdin, mut out, mut err) = (
        child.stdin.take().expect("piped"),
        child.stdout.take().expect("piped"),
        child.stderr.take().expect("piped"),
    );
    let input = input.to_owned();
    thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let err = thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let until = Instant::now() + timeout;
    let text = |t: thread::JoinHandle<Vec<u8>>| {
        String::from_utf8_lossy(&t.join().unwrap_or_default()).into_owned()
    };
    let stop = |child: &mut std::process::Child| {
        if let Some(pid) = rustix::process::Pid::from_raw(child.id() as i32) {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    };
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if paused() {
            stop(&mut child);
            let output = text(out);
            let _ = text(err);
            return Ok(Ran::Paused(output));
        }
        if Instant::now() >= until {
            // Whatever it started may hold its output open: that's left be.
            stop(&mut child);
            return Ok(Ran::TimedOut);
        }
        thread::sleep(Duration::from_millis(50));
    };
    Ok(Ran::Ended(
        status.code().unwrap_or(-1),
        text(out),
        text(err),
    ))
}

/// The JSON events an agent printed, one a line (--json).
pub fn events(out: &str) -> Vec<Value> {
    out.lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(Value::is_object)
        .collect()
}

/// The JSON object an agent ends with.
pub fn answer_of(text: &str, keys: &[String]) -> Result<Value, Failed> {
    let found = match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if a < b => serde_json::from_str(&text[a..=b]).ok(),
        _ => None,
    };
    found.ok_or_else(|| {
        Failed::Error(format!(
            "no JSON in its answer: {:?}",
            tail(text, 200, keys)
        ))
    })
}

/// A weekday and time, UTC: "Sat 14:03Z".
fn when(at: f64) -> String {
    jiff::Timestamp::from_second(at as i64)
        .map_or_else(|_| format!("{at}"), |t| t.strftime("%a %H:%MZ").to_string())
}

/// Codex's look in the PR's session `sid` (a new one if None): its answer,
/// and the session's id. `home` is the PR's own CODEX_HOME, which holds
/// that session and nothing of any other PR's but the login, a copy lent
/// for this look (login.rs).
pub fn look(
    s: &Service,
    snap: &Path,
    prompt: &str,
    home: &Path,
    sid: Option<&str>,
    repo: &str,
) -> Result<(Value, String), Failed> {
    let err = |e: io::Error| Failed::Error(e.to_string());
    let login = s.home.join(".codex");
    fs::create_dir_all(home).map_err(err)?;
    let f = "config.toml";
    if login.join(f).exists() && fs::symlink_metadata(home.join(f)).is_err() {
        symlink(login.join(f), home.join(f)).map_err(err)?;
    }
    let lent = s.logins.lend(home).map_err(err)?;
    let t = tempfile::tempdir().map_err(err)?;
    let t = t.path();
    fs::write(t.join("schema.json"), schema().to_string()).map_err(err)?;
    let quoted = |p: &Path| Value::String(p.to_string_lossy().into_owned()).to_string();
    let mut flags: Vec<OsString> = vec![
        "--json".into(),
        "--output-schema".into(),
        t.join("schema.json").into(),
    ];
    flags.extend(["-o".into(), t.join("answer.json").into()]);
    for c in [
        // The commands it runs read everything but its homes (its login)
        // and write nothing.
        r#"default_permissions="review""#.to_owned(),
        format!(
            r#"permissions.review.filesystem={{":root"="read", {}="none", {}="none"}}"#,
            quoted(home),
            quoted(&login)
        ),
        // The checkout's AGENTS.md is the PR's own: the base's rules are in the brief.
        "project_doc_max_bytes=0".to_owned(),
        format!("model_reasoning_effort={}", s.cfg.reviewers.codex.effort),
    ] {
        flags.extend(["-c".into(), c.into()]);
    }
    let codex: OsString = s.cfg.agents.join("codex").into();
    let argv: Vec<OsString> = match sid {
        Some(sid) => [
            vec![codex, "exec".into(), "resume".into()],
            flags,
            vec![sid.into(), "-".into()],
        ]
        .concat(),
        None => [
            vec![
                codex,
                "exec".into(),
                "--cd".into(),
                CHECKOUT.into(),
                "--color".into(),
                "never".into(),
            ],
            flags,
            vec!["-".into()],
        ]
        .concat(),
    };
    // The machine's settings.
    let config = login.join(f);
    let mounts: Vec<(&str, PathBuf)> = if config.exists() {
        vec![("--ro-bind", config)]
    } else {
        Vec::new()
    };
    let args = sandboxed(s, snap, home, t, &["HTTPS_PROXY"], &argv, &mounts).map_err(err)?;
    let mut cmd = Command::new(&s.cfg.sandbox);
    cmd.args(args).env_clear().envs([
        ("HOME", s.home.as_os_str()),
        ("PATH", s.cfg.path.as_ref()),
        ("LANG", "C.UTF-8".as_ref()),
        ("SHELL", "/bin/bash".as_ref()),
        (
            "SSL_CERT_FILE",
            "/etc/ssl/certs/ca-certificates.crt".as_ref(),
        ),
        ("CODEX_HOME", home.as_os_str()),
    ]);
    let gate = Gate::open(&t.join("egress.sock"), HOSTS, s.dial.clone()).map_err(err)?;
    let ran = run(
        cmd,
        prompt,
        Duration::from_secs_f64(s.cfg.timeout_minutes * 60.0),
        || s.cfg.paused(repo),
    );
    drop(gate);
    lent.give_back()
        .map_err(|e| Failed::Error(format!("couldn't keep its login: {e}")))?;
    // Every key it can have seen, with any it refreshed.
    let keys = s.keys();
    let (code, out, stderr) = match ran.map_err(err)? {
        Ran::Ended(code, out, stderr) => (code, out, stderr),
        Ran::Paused(out) => {
            let session = events(&out)
                .into_iter()
                .find(|e| e["type"] == "thread.started")
                .and_then(|e| e["thread_id"].as_str().map(str::to_owned))
                .or_else(|| sid.map(str::to_owned));
            return Err(Failed::Paused { session });
        }
        Ran::TimedOut => {
            return Err(Failed::Error(format!(
                "codex was still at it after {} minutes",
                s.cfg.timeout_minutes
            )));
        }
    };
    // What went wrong, in its own words: stderr and the error events. Its
    // other events hold what it read (the PR's files among them), which can
    // say anything.
    let events = events(&out);
    let mut said = stderr;
    for e in &events {
        if matches!(e["type"].as_str(), Some("error" | "turn.failed")) {
            let m = e["message"]
                .as_str()
                .filter(|m| !m.is_empty())
                .or(e["error"]["message"].as_str());
            said.push('\n');
            said.push_str(m.unwrap_or(""));
        }
    }
    if code != 0 && spent(&said) {
        // Asked again within the hour, whatever the plan's reset: credits
        // (or a bigger plan) carry it on before then.
        let back = now() + wait_for(&said);
        return Err(Failed::Out {
            why: format!(
                "its ChatGPT plan's usage is spent, back {} unless credits carry it",
                when(back)
            ),
            until: back.min(now() + 3600.0),
        });
    }
    if code != 0 && logged_out(&said) {
        return Err(Failed::Out {
            why: "it isn't logged in (autoland-codex-login review on the machine)".into(),
            until: now() + 3600.0,
        });
    }
    if code != 0 {
        return Err(Failed::Error(format!(
            "codex exited {code}: {}",
            tail(said.trim(), 300, &keys)
        )));
    }
    let sid = events
        .iter()
        .find(|e| e["type"] == "thread.started")
        .and_then(|e| e["thread_id"].as_str())
        .or(sid)
        .ok_or_else(|| Failed::Error("codex named no session".into()))?
        .to_owned();
    let answer = fs::read_to_string(t.join("answer.json"))
        .map_err(|e| Failed::Error(format!("its answer: {e}")))?;
    Ok((answer_of(&answer, &keys)?, sid))
}

#[cfg(test)]
mod tests {
    #[test]
    fn reset_time_is_shown_in_utc() {
        assert_eq!(super::when(1_793_040_155.0), "Mon 18:42Z");
    }
}
