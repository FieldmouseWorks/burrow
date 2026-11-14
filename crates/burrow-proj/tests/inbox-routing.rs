//! Run the production App wrapper against a fake token socket and GitHub.
use std::io::{Read, Write};
use std::os::unix::{fs::symlink, net::UnixListener};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use burrow_proj::{config::Config, github};
use serde_json::json;

fn config(gh: &Path) -> Config {
    serde_json::from_value(json!({
        "projects":"/unused", "jobs":"/unused", "scratch":"/unused",
        "slice":"unused", "maxJobs":1, "pipelineSlots":1, "maxJobsPerProject":1,
        "minFreeMemory":0, "jobMemoryMax":"1G", "timeout":"1m", "maxWait":1,
        "stallAfterMinutes":1, "awaitHours":1, "kick":"", "wake":"/unused",
        "waits":null, "inboxMark":"", "pipelineRepos":[], "leadRepos":[],
        "flake":"/unused", "deployed":"/unused", "apps":{}, "machinery":null,
        "notify":null, "meters":null, "codexSlots":1,
        "codexHooks":"/unused", "inbox":null,
        "tools":{"gh":gh, "git":"git", "codex":"unused", "bwrap":"unused",
            "bash":"unused", "flock":"unused", "systemctl":"unused",
            "systemdRun":"unused", "timeout":"unused"}
    }))
    .unwrap()
}

fn main() {
    let exe = std::env::current_exe().unwrap();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if std::env::args().next().unwrap().ends_with("wrapped-gh") {
        let socket = std::env::var_os("GH_TOKEN_SOCKET").unwrap();
        let error = gh_token::client::gh(exe.to_str().unwrap(), &args, Some(Path::new(&socket)));
        eprintln!("{error}");
        std::process::exit(1);
    }
    match args.first().map(String::as_str) {
        Some("help") => {
            // The wrapper parses the real CLI's help, exactly as deployed.
            let status = Command::new("gh")
                .args(&args)
                .env("GH_TOKEN", "test-help-token")
                .status()
                .unwrap();
            std::process::exit(status.code().unwrap_or(1));
        }
        Some("api") => {
            println!(
                "HTTP/2.0 200 OK\n\n{}",
                json!({"data":{"token":std::env::var("GH_TOKEN").unwrap()}})
            );
            return;
        }
        Some("case") => {
            let cfg = config(Path::new(&args[2]));
            for cursor in [None, Some("next-page")] {
                let got = github::query_for(
                    &cfg,
                    &args[1],
                    "query($owner:String!){viewer{login}}",
                    json!({"owner":"unrelated","cursor":cursor}),
                )
                .expect("authenticated GraphQL");
                assert_eq!(got["token"], "test-installation-token");
            }
            return;
        }
        _ => {}
    }
    let dir = tempfile::tempdir().unwrap();
    let wrapped = dir.path().join("wrapped-gh");
    symlink(&exe, &wrapped).unwrap();
    for owner in ["FieldmouseWorks", "TusanHomichi"] {
        for inherited in ["", "wrong/repo"] {
            let socket = dir.path().join("socket");
            let listener = UnixListener::bind(&socket).unwrap();
            listener.set_nonblocking(true).unwrap();
            let serving = std::thread::spawn(move || {
                let mut asked = Vec::new();
                let deadline = Instant::now() + Duration::from_secs(10);
                while asked.len() < 2 && Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_read_timeout(Some(Duration::from_secs(2)))
                                .unwrap();
                            let mut request = String::new();
                            stream.read_to_string(&mut request).unwrap();
                            asked.push(request);
                            stream
                                .write_all(b"token test-installation-token\n")
                                .unwrap();
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => panic!("{e}"),
                    }
                }
                asked
            });
            let result = burrow_test::command(&exe)
                .args(["case", &format!("{owner}/repo"), wrapped.to_str().unwrap()])
                .current_dir(dir.path()) // No repository or useful checkout.
                .env_remove("GH_TOKEN")
                .env_remove("GITHUB_TOKEN")
                .env_remove("GH_HOST")
                .env("GH_REPO", inherited)
                .env("GH_TOKEN_OWNER", "wrong")
                .env("GH_TOKEN_SOCKET", &socket)
                .output()
                .unwrap();
            assert_eq!(serving.join().unwrap(), vec![format!("{owner}\n"); 2]);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            std::fs::remove_file(socket).unwrap();
        }
    }
}
