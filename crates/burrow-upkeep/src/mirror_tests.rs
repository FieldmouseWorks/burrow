//! The steward's mirror against a GitHub that stops answering.

use std::io::Read;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::testing::config;

/// A GitHub that takes the fetch's connection, reads what it asks and then
/// says nothing, as the one a pass waited on until its unit's deadline
/// killed it did (burrow#634): the fetch gives up once it has heard nothing
/// for its stall, with the pass's own recovery still ahead of it.
#[test]
fn a_fetch_github_stops_answering_gives_up_after_its_stall() {
    let d = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    thread::spawn(move || {
        let mut held = vec![];
        for conn in listener.incoming() {
            let Ok(mut conn) = conn else { return };
            let mut buf = [0; 4096];
            let _ = conn.read(&mut buf);
            held.push(conn); // open, and silent
        }
    });
    let mut cfg = config(d.path());
    cfg.git = format!("http://127.0.0.1:{port}");
    let state = d.path().to_path_buf();
    let (tx, rx) = mpsc::channel();
    let started = Instant::now();
    thread::spawn(move || {
        let refs = ["+refs/pull/5/head:refs/pr/5".to_string()];
        let _ = tx.send(fetch_within(&cfg, &state, "sam/r", &refs, 2));
    });
    let got = rx
        .recv_timeout(Duration::from_secs(60))
        .expect("the fetch waited on a silent GitHub with no end");
    let why = got.unwrap_err();
    assert!(why.starts_with("git -C "), "{why}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
    // What a pass's fetch waits for at most: under the two minutes it
    // gives each of its other calls to GitHub (common.py REMOTE_TIMEOUT).
    const { assert!(STALL <= 120) };
}
