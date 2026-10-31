//! Codex's look: the codex CLI, read-only in a sandbox (bubblewrap) that
//! shows it its snapshot and its PR's session and little else, with no
//! network but its own API through the gate (egress.rs).

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use serde_json::{Value, json};

use crate::egress::Gate;
use crate::scrub::tail;
use crate::{Failed, Service, now};

/// Where a look sees its snapshot: the same path for every head, so a
/// session resumed on a PR's new head finds the checkout where it left it.
pub const CHECKOUT: &str = "/review";

/// Where Codex's API is: it goes through the gate as its HTTPS proxy, to
/// these hosts only.
pub const HOSTS: &[&str] = &["chatgpt.com", "auth.openai.com", "api.openai.com"];

/// The answer it's held to, which the steward posts (reviewers.py `post`).
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
fn run(
    mut cmd: Command,
    input: &str,
    timeout: Duration,
) -> io::Result<Option<(i32, String, String)>> {
    let mut child = cmd
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
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if Instant::now() >= until {
            // Whatever it started may hold its output open: that's left be.
            let _ = child.kill();
            let _ = child.wait();
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(50));
    };
    let text = |t: thread::JoinHandle<Vec<u8>>| {
        String::from_utf8_lossy(&t.join().unwrap_or_default()).into_owned()
    };
    Ok(Some((status.code().unwrap_or(-1), text(out), text(err))))
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

/// When a usage limit says to try again, in seconds: "try again in 2 days 3
/// hours", else an hour on; never less than a minute.
pub fn wait_for(text: &str) -> f64 {
    const AT: &str = "try again in ";
    let Some(i) = text.find(AT) else {
        return 3600.0;
    };
    let mut rest = &text[i + AT.len()..];
    let mut total: Option<u64> = None;
    loop {
        let digits = rest.len() - rest.trim_start_matches(|c: char| c.is_ascii_digit()).len();
        let Ok(n) = rest[..digits].parse::<u64>() else {
            break;
        };
        let after = rest[digits..].trim_start();
        let word = after.len()
            - after
                .trim_start_matches(|c: char| c.is_ascii_lowercase())
                .len();
        let unit = match &after[..word] {
            "day" | "days" => 86400,
            "hour" | "hours" => 3600,
            "minute" | "minutes" => 60,
            "second" | "seconds" => 1,
            _ => break,
        };
        total = Some(total.unwrap_or(0).saturating_add(n.saturating_mul(unit)));
        rest = after[word..].trim_start_matches(|c: char| c == ',' || c.is_whitespace());
        if let Some(more) = rest.strip_prefix("and")
            && more.starts_with(char::is_whitespace)
        {
            rest = more.trim_start();
        }
    }
    total.map_or(3600.0, |t| t.max(60) as f64)
}

/// Whether text has `word` with no letter, digit or underscore either side.
fn has_word(text: &str, word: &str) -> bool {
    let wordy = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    text.match_indices(word).any(|(i, _)| {
        !wordy(text[..i].chars().next_back()) && !wordy(text[i + word.len()..].chars().next())
    })
}

/// The plan's usage spent.
fn spent(said: &str) -> bool {
    let said = said.to_lowercase();
    said.contains("usage limit") || said.contains("usage_limit_reached")
}

/// Its login gone or refused.
fn logged_out(said: &str) -> bool {
    let said = said.to_lowercase();
    [
        "not logged in",
        "login again",
        "log in again",
        "unauthorized",
        "refresh token",
    ]
    .iter()
    .any(|x| said.contains(x))
        || has_word(&said, "401")
}

/// A weekday and time, UTC: "Sat 14:03Z".
fn when(at: f64) -> String {
    jiff::Timestamp::from_second(at as i64)
        .map_or_else(|_| format!("{at}"), |t| t.strftime("%a %H:%MZ").to_string())
}

/// Codex's look in the PR's session `sid` (a new one if None): its answer,
/// and the session's id. `home` is the PR's own CODEX_HOME, which holds
/// that session and nothing of any other PR's; the login in it is the
/// reviewers' one (HOME/.codex/auth.json, linked in).
pub fn look(
    s: &Service,
    snap: &Path,
    prompt: &str,
    home: &Path,
    sid: Option<&str>,
) -> Result<(Value, String), Failed> {
    let keys = s.keys();
    let err = |e: io::Error| Failed::Error(e.to_string());
    let login = s.home.join(".codex");
    fs::create_dir_all(home).map_err(err)?;
    for f in ["auth.json", "config.toml"] {
        if login.join(f).exists() && fs::symlink_metadata(home.join(f)).is_err() {
            symlink(login.join(f), home.join(f)).map_err(err)?;
        }
    }
    // The session as it is now, wherever it goes: a PR that closes while
    // Codex looks has its session marked gone (reviewers.py `sweep`), and a
    // refresh saved in it still goes back to the login.
    let held = rustix::fs::open(
        home,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| err(e.into()))?;
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
    // The login, where a refresh writes it, and the machine's settings.
    let mounts: Vec<(&str, PathBuf)> = [("--bind", "auth.json"), ("--ro-bind", "config.toml")]
        .into_iter()
        .map(|(flag, f)| (flag, login.join(f)))
        .filter(|(_, p)| p.exists())
        .collect();
    let args = sandboxed(s, snap, home, t, &["HTTPS_PROXY"], &argv, &mounts).map_err(err)?;
    let mut cmd = Command::new(&s.cfg.sandbox);
    cmd.args(args).env_clear().envs([
        ("HOME", s.home.as_os_str()),
        ("PATH", s.path.as_os_str()),
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
    );
    drop(gate);
    keep_login(s, &held, &login)
        .map_err(|e| Failed::Error(format!("couldn't keep its login: {e}")))?;
    let Some((code, out, stderr)) = ran.map_err(err)? else {
        return Err(Failed::Error(format!(
            "codex was still at it after {} minutes",
            s.cfg.timeout_minutes
        )));
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
            why: "it isn't logged in (autoland-codex-login on the machine)".into(),
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

/// The reviewers' login as Codex last saved it: one that saved it in place
/// of the PR's link to it (a refresh written as a new file) puts it back
/// where every PR's session reads it. `home` is the session's CODEX_HOME,
/// open, so a session renamed meanwhile is still the one read.
fn keep_login(s: &Service, home: &OwnedFd, login: &Path) -> io::Result<()> {
    let st = match rustix::fs::statat(home, "auth.json", AtFlags::SYMLINK_NOFOLLOW) {
        Ok(st) => st,
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if FileType::from_raw_mode(st.st_mode) != FileType::RegularFile {
        return Ok(());
    }
    let _one = s.login.lock().unwrap_or_else(|e| e.into_inner());
    let mut src = File::from(rustix::fs::openat(
        home,
        "auth.json",
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let tmp = login.join("auth.json.tmp");
    let mut dst = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    io::copy(&mut src, &mut dst)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
    fs::rename(&tmp, login.join("auth.json"))?;
    rustix::fs::unlinkat(home, "auth.json", AtFlags::empty())?;
    rustix::fs::symlinkat(login.join("auth.json"), home, "auth.json")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{has_word, logged_out, spent, wait_for, when};

    /// A usage limit's own word for when, or an hour; a minute at least.
    #[test]
    fn when_a_limit_says_to_try_again() {
        assert_eq!(
            wait_for("Upgrade to Pro or try again in 2 days 3 hours."),
            (2.0 * 86400.0) + 3.0 * 3600.0
        );
        assert_eq!(wait_for("try again in 20 minutes"), 1200.0);
        assert_eq!(wait_for("try again in 1 hour, and 30 minutes"), 5400.0);
        assert_eq!(wait_for("try again in 1 day and 2 hours"), 93600.0);
        assert_eq!(wait_for("try again in 5 seconds"), 60.0);
        assert_eq!(wait_for("try again in 2days"), 172800.0);
        for none in [
            "try again later",
            "try again in a bit",
            "Try again in 2 hours",
            "",
        ] {
            assert_eq!(wait_for(none), 3600.0, "{none}");
        }
    }

    /// Spent and logged out by what codex says; a 401 inside a longer
    /// number is neither.
    #[test]
    fn out_of_service_by_its_own_words() {
        assert!(spent("You've hit your usage limit.") && spent("error: USAGE_LIMIT_REACHED"));
        assert!(!spent("no usage of limits"));
        for out in [
            "Error: Not logged in.",
            "please log in again",
            "Login again",
            "HTTP 401",
            "401: Unauthorized",
            "bad refresh token",
        ] {
            assert!(logged_out(out), "{out}");
        }
        for not in ["no session session-1793040155", "x401", "4010", "_401"] {
            assert!(!logged_out(not), "{not}");
        }
        assert!(has_word("(401)", "401") && has_word("401", "401"));
        assert_eq!(when(1_793_040_155.0), "Mon 18:42Z");
    }
}
