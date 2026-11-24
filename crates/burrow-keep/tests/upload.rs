//! burrow-keep-upload as the builds-write step runs it, on a store of its
//! own with one path no one signed: a job with no token can't write and
//! says so without failing; one whose store doesn't answer fails, which
//! the step reports as its degradation, and nothing more. Needs nix on
//! PATH, as the flake's check has it.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::process::{Command, Output};

fn upload(dir: &std::path::Path, url: &str, token: Option<&str>) -> Output {
    let config = format!(
        "experimental-features = nix-command\nstore = local?root={}\nsubstituters =",
        dir.join("root").display()
    );
    let mut c = Command::new(burrow_test::program!("burrow-keep-upload"));
    c.arg(url)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", dir)
        .env("NIX_CONFIG", &config)
        .env("GITHUB_STEP_SUMMARY", dir.join("summary"));
    if let Some(t) = token {
        c.env(
            "ACTIONS_ID_TOKEN_REQUEST_URL",
            format!("{t}?api-version=2.0"),
        )
        .env("ACTIONS_ID_TOKEN_REQUEST_TOKEN", "request");
    }
    // One path the run built: a directory, so not a download (a flat
    // file), which only goes when something refers to it.
    let built = dir.join("built");
    fs::create_dir_all(&built).unwrap();
    fs::write(built.join("out"), "built here\n").unwrap();
    let added = Command::new("nix")
        .args(["store", "add-path", built.to_str().unwrap()])
        .env("NIX_CONFIG", &config)
        .env("HOME", dir)
        .output()
        .unwrap();
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    c.output().unwrap()
}

/// Actions' token service, answering every request with a token.
fn tokens() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/token", l.local_addr().unwrap());
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            while r.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                line.clear();
            }
            let body = r#"{"value":"a-token"}"#;
            write!(
                s,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        }
    });
    url
}

#[test]
fn a_job_with_no_token_says_it_cant_write_and_doesnt_fail() {
    let t = tempfile::tempdir().unwrap();
    let o = upload(t.path(), "https://builds.example/builds", None);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let said = String::from_utf8_lossy(&o.stdout);
    assert!(said.contains("has no OIDC token"), "{said}");
    let summary = fs::read_to_string(t.path().join("summary")).unwrap();
    assert!(summary.contains("has no OIDC token"), "{summary}");
}

#[test]
fn a_store_that_doesnt_answer_fails_the_step() {
    let t = tempfile::tempdir().unwrap();
    let closed = burrow_test::ClosedPort::new();
    let url = format!("http://{}/builds", closed.addr());
    let o = upload(t.path(), &url, Some(&tokens()));
    assert!(!o.status.success());
    let said = String::from_utf8_lossy(&o.stderr);
    assert!(said.contains("didn't answer"), "{said}");
}
