//! End-to-end proof that `burrow-flake-check` handles JSON via serde without
//! ever invoking a `jq` subprocess.
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;

fn which(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join(name))
        .find(|p| p.is_file())
        .unwrap_or_else(|| panic!("no {name} on PATH"))
}

#[test]
fn reuse_handles_json_without_jq() {
    let dir = tempfile::tempdir().unwrap();
    // Construct a minimal PATH containing only the tools `reuse` legitimately
    // invokes, with no `jq` binary present anywhere on PATH.
    let bin = dir.path().join("bin");
    fs::create_dir_all(&bin).unwrap();
    for tool in ["curl", "sha256sum", "mktemp", "chmod"] {
        symlink(which(tool), bin.join(tool)).unwrap();
    }

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let (mut conn, _) = listener.accept().unwrap();
            let mut reader = BufReader::new(conn.try_clone().unwrap());
            let mut req_line = String::new();
            reader.read_line(&mut req_line).unwrap();
            let path = req_line.split_whitespace().nth(1).unwrap().to_owned();
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if reader.read_line(&mut h).unwrap() <= 2 {
                    break;
                }
                if let Some((k, v)) = h.split_once(':')
                    && k.eq_ignore_ascii_case("content-length")
                {
                    len = v.trim().parse().unwrap();
                }
            }
            let mut body = vec![0u8; len];
            reader.read_exact(&mut body).unwrap();
            let reply = if path.starts_with("/token") {
                r#"{"count":1,"value":"tok"}"#.to_owned()
            } else {
                let req: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(req["contract"], "burrow-gate-reuse/v1");
                assert_eq!(req["gate"]["a"], 1);
                let out = Command::new("sha256sum")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .spawn()
                    .and_then(|mut c| {
                        c.stdin.take().unwrap().write_all(&body)?;
                        c.wait_with_output()
                    })
                    .unwrap();
                let query = String::from_utf8(out.stdout)
                    .unwrap()
                    .split_whitespace()
                    .next()
                    .unwrap()
                    .to_owned();
                serde_json::json!({
                    "contract": "burrow-gate-reuse/v1",
                    "query": query,
                    "evidence": "hit",
                    "run": "https://github.com/owner/repo/actions/runs/42"
                })
                .to_string()
            };
            write!(
                conn,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                reply.len()
            )
            .unwrap();
        }
    });

    let runner_temp = dir.path().join("temp");
    fs::create_dir_all(&runner_temp).unwrap();
    fs::write(runner_temp.join("gate.json"), r#"{"z": 2, "a": 1}"#).unwrap();
    let github_output = dir.path().join("output");
    fs::write(&github_output, "").unwrap();

    let commit = "1".repeat(40);
    let tree = "2".repeat(40);
    let out = Command::new(burrow_test::program!("burrow-flake-check"))
        .arg("reuse")
        .env("PATH", &bin)
        .env("RUNNER_TEMP", &runner_temp)
        .env("GITHUB_OUTPUT", &github_output)
        .env("LOOKUP", format!("http://{addr}/gate"))
        .env(
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            format!("http://{addr}/token?v=1"),
        )
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "req-tok")
        .env("GITHUB_REF", "refs/heads/main")
        .env("DEFAULT_BRANCH", "main")
        .env("GITHUB_REPOSITORY", "owner/repo")
        .env("GITHUB_SERVER_URL", "https://github.com")
        .env("COMMIT", &commit)
        .env("HEAD_ID", &commit)
        .env("TREE", &tree)
        .output()
        .unwrap();
    let outputs = fs::read_to_string(&github_output).unwrap();
    assert!(
        out.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    server.join().unwrap();
    assert_eq!(
        outputs,
        "hit=true\nrun=https://github.com/owner/repo/actions/runs/42\n"
    );
}
