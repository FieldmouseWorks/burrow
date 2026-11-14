//! Exercise the real CLI and host handler with disjoint caller/host config.
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::net::UnixListener,
    path::Path,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

const BIN: &str = env!("CARGO_BIN_EXE_burrow-units");

fn config(path: &Path, state: &Path, socket: &Path, own: &str) {
    fs::write(
        path,
        serde_json::to_vec(&serde_json::json!({
            "file":state.join("watch.json"), "clearSocket":socket,
            "ignore":[], "grace":600, "window":86400, "systemctl":"false",
            "journalctl":"false", "buildGid":null, "ownedSlices":[], "own":own, "kick":null
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn isolated_caller_acknowledges_on_host_and_reports_a_failed_pass_request() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let host = root.join("host");
    fs::create_dir(&host).unwrap();
    let socket = root.join("clear.sock");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let requests = seen.clone();
    let (port, _) = burrow_events::http::listen(
        0,
        Arc::new(move |request| {
            requests.lock().unwrap().push(request.target.clone());
            if requests.lock().unwrap().len() == 1 {
                202
            } else {
                503
            }
        }),
    )
    .unwrap();
    let host_config = root.join("host.json");
    config(
        &host_config,
        &host,
        &socket,
        &format!("http://127.0.0.1:{port}/kick/units"),
    );
    let caller_config = root.join("caller.json");
    // Both state and event URL are inaccessible/wrong on the caller; neither
    // may be used by a clear that successfully reached the host.
    config(
        &caller_config,
        &root.join("absent"),
        &socket,
        "http://127.0.0.1:1/wrong",
    );
    let listener = UnixListener::bind(&socket).unwrap();
    let serve = thread::spawn(move || {
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).unwrap();
            let mut handler = Command::new(BIN)
                .arg("serve-clear")
                .env("UNITS_CONFIG", &host_config)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            handler.stdin.take().unwrap().write_all(&bytes).unwrap();
            let result = handler.wait_with_output().unwrap();
            assert!(result.status.success());
            stream.write_all(&result.stdout).unwrap();
        }
    });
    let exe = "/bin/program with spaces; $(never-run)";
    let clear = || {
        Command::new(BIN)
            .args(["clear", exe])
            .env("UNITS_CONFIG", &caller_config)
            .output()
            .unwrap()
    };
    // The host clear must wait for the very lock a watch pass holds.
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(host.join("lock"))
        .unwrap();
    lock.lock().unwrap();
    let (tx, rx) = mpsc::channel();
    let caller = caller_config.clone();
    let pending = thread::spawn(move || {
        tx.send(
            Command::new(BIN)
                .args(["clear", exe])
                .env("UNITS_CONFIG", caller)
                .output()
                .unwrap(),
        )
        .unwrap();
    });
    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    assert!(!host.join("cleared.json").exists());
    drop(lock);
    let first = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    pending.join().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let cleared: BTreeMap<String, f64> =
        serde_json::from_slice(&fs::read(host.join("cleared.json")).unwrap()).unwrap();
    assert!(cleared[exe] > 0.0);
    assert!(!root.join("absent").exists());
    let second = clear();
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("HTTP 503"));
    serve.join().unwrap();
    assert_eq!(*seen.lock().unwrap(), ["/kick/units", "/kick/units"]);
}
