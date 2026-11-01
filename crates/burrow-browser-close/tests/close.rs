//! Closing the browser against a stand-in DevTools: `Browser.close` sent as
//! a client's masked frame on the browser's own WebSocket, then a wait for
//! its main process to exit.

mod common;

use std::time::{Duration, Instant};

use base64::Engine as _;
use burrow_browser_close::{Closed, close};
use common::{Answer, DevTools, browser, nobody, payload};
use serde_json::{Value, json};

#[test]
fn it_says_browser_close_and_waits_for_the_browser_to_exit() {
    let b = browser();
    let pid = b.id();
    let devtools = DevTools::serve(Answer::Closes, Some(b));
    assert_eq!(
        close(devtools.port, pid, Duration::from_secs(10)),
        Ok(Closed::Exited)
    );
    let seen = devtools.seen();
    let port = devtools.port;
    assert!(
        seen.version.starts_with(&format!(
            "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n"
        )),
        "{}",
        seen.version
    );
    let upgrade: Vec<&str> = seen.upgrade.split("\r\n").collect();
    assert_eq!(upgrade[0], "GET /devtools/browser/0b4e HTTP/1.1");
    for header in [
        format!("Host: 127.0.0.1:{port}"),
        "Upgrade: websocket".to_owned(),
        "Connection: Upgrade".to_owned(),
        "Sec-WebSocket-Version: 13".to_owned(),
    ] {
        assert!(upgrade.contains(&header.as_str()), "{header}: {upgrade:?}");
    }
    let key = upgrade
        .iter()
        .find_map(|h| h.strip_prefix("Sec-WebSocket-Key: "))
        .expect("a key");
    let key = base64::engine::general_purpose::STANDARD
        .decode(key)
        .unwrap();
    assert_eq!(key.len(), 16);
    // One final text frame, masked as a client's must be.
    assert_eq!(seen.frame[0], 0x81);
    assert_eq!(seen.frame[1] & 0x80, 0x80);
    let sent = payload(&seen.frame);
    assert_eq!(usize::from(seen.frame[1] & 0x7f), sent.len());
    let sent: Value = serde_json::from_slice(&sent).unwrap();
    assert_eq!(sent, json!({"id": 1, "method": "Browser.close"}));
}

#[test]
fn each_close_has_a_key_and_mask_of_its_own() {
    let mut keys = Vec::new();
    let mut masks = Vec::new();
    for _ in 0..2 {
        let b = browser();
        let pid = b.id();
        let devtools = DevTools::serve(Answer::Closes, Some(b));
        assert_eq!(
            close(devtools.port, pid, Duration::from_secs(10)),
            Ok(Closed::Exited)
        );
        let seen = devtools.seen();
        keys.push(seen.upgrade.split("Sec-WebSocket-Key: ").nth(1).unwrap()[..24].to_owned());
        masks.push(seen.frame[2..6].to_vec());
    }
    assert_ne!(keys[0], keys[1]);
    assert_ne!(masks[0], masks[1]);
}

/// Its answer says where it ends, and what follows on the connection isn't
/// part of it.
#[test]
fn an_answer_is_read_to_its_length() {
    let b = browser();
    let pid = b.id();
    let devtools = DevTools::serve(Answer::Trailing, Some(b));
    assert_eq!(
        close(devtools.port, pid, Duration::from_secs(10)),
        Ok(Closed::Exited)
    );
}

#[test]
fn with_nothing_answering_as_devtools_theres_nothing_to_close() {
    let started = Instant::now();
    assert_eq!(
        close(nobody(), 1, Duration::from_secs(10)),
        Ok(Closed::NotAnswering)
    );
    for status in [404, 500] {
        let devtools = DevTools::serve(Answer::Version(status), None);
        assert_eq!(
            close(devtools.port, 1, Duration::from_secs(10)),
            Ok(Closed::NotAnswering),
            "{status}"
        );
        assert!(devtools.seen().upgrade.is_empty(), "{status}");
    }
    // PID 1 is always there: none of this waited for it.
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn an_answer_it_cant_read_is_a_failure() {
    for (body, why) in [
        (
            "<html>not DevTools</html>",
            "DevTools' /json/version isn't JSON: ",
        ),
        (
            r#"{"Browser": "Chrome/140.0"}"#,
            "DevTools' /json/version names no webSocketDebuggerUrl",
        ),
        (
            r#"{"webSocketDebuggerUrl": "ws://127.0.0.1:1/devtools/browser/0b4e"}"#,
            "DevTools' WebSocket, ws://127.0.0.1:1/devtools/browser/0b4e, isn't on port ",
        ),
    ] {
        let devtools = DevTools::serve(Answer::Body(body), None);
        let failed = close(devtools.port, 1, Duration::from_secs(10)).unwrap_err();
        assert!(failed.starts_with(why), "{failed}");
        assert!(devtools.seen().upgrade.is_empty(), "{body}");
    }
}

#[test]
fn a_refused_websocket_says_what_devtools_said() {
    let devtools = DevTools::serve(Answer::Refuses, None);
    assert_eq!(
        close(devtools.port, 1, Duration::from_secs(10)),
        Err("CDP refused the WebSocket: HTTP/1.1 400 Bad Request".to_owned())
    );
}

#[test]
fn a_browser_still_up_when_its_time_is_up_is_a_failure() {
    let mut b = browser();
    let devtools = DevTools::serve(Answer::StaysUp, None);
    let started = Instant::now();
    let failed = close(devtools.port, b.id(), Duration::from_secs(1));
    let took = started.elapsed();
    b.kill().unwrap();
    b.wait().unwrap();
    assert_eq!(
        failed,
        Err("the browser was still up 1s after Browser.close".to_owned())
    );
    assert!(took >= Duration::from_secs(1), "{took:?}");
    assert!(took < Duration::from_secs(3), "{took:?}");
    assert!(!devtools.seen().frame.is_empty());
}
