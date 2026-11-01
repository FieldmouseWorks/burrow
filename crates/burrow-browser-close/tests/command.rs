//! burrow-browser-close as the browser unit's ExecStop= runs it: the port
//! as its word, the unit's main process in MAINPID.

mod common;

use std::process::{Command, Output};
use std::time::{Duration, Instant};

use common::{Answer, DevTools, browser, nobody};

fn run(args: &[&str], pid: Option<&str>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_burrow-browser-close"));
    c.args(args).env_remove("MAINPID");
    if let Some(pid) = pid {
        c.env("MAINPID", pid);
    }
    c.output().unwrap()
}

#[test]
fn it_closes_the_browser_and_exits_once_its_gone() {
    let b = browser();
    let pid = b.id().to_string();
    let devtools = DevTools::serve(Answer::Closes, Some(b));
    let started = Instant::now();
    let out = run(&[&devtools.port.to_string()], Some(&pid));
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!devtools.seen().frame.is_empty());
}

#[test]
fn with_nothing_answering_it_exits_0_saying_nothing() {
    let out = run(&[&nobody().to_string()], Some("1"));
    assert!(out.status.success(), "{out:?}");
    assert!(out.stdout.is_empty() && out.stderr.is_empty(), "{out:?}");
}

#[test]
fn a_failure_exits_1_and_says_why() {
    let devtools = DevTools::serve(Answer::Refuses, None);
    let out = run(&[&devtools.port.to_string()], Some("1"));
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "burrow-browser-close: CDP refused the WebSocket: HTTP/1.1 400 Bad Request\n"
    );
}

#[test]
fn what_it_cant_run() {
    for (args, pid, says) in [
        (&[][..], Some("1"), "usage: "),
        (&["9222", "9223"][..], Some("1"), "usage: "),
        (&["http"][..], Some("1"), "usage: "),
        (&["70000"][..], Some("1"), "usage: "),
        (
            &["9222"][..],
            None,
            "burrow-browser-close: MAINPID isn't a process ID",
        ),
        (
            &["9222"][..],
            Some(""),
            "burrow-browser-close: MAINPID isn't a process ID",
        ),
        (
            &["9222"][..],
            Some("../1"),
            "burrow-browser-close: MAINPID isn't a process ID",
        ),
    ] {
        let out = run(args, pid);
        assert_eq!(out.status.code(), Some(2), "{args:?} {pid:?}");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.starts_with(says), "{args:?} {pid:?}: {said}");
    }
}
