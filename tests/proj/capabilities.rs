//! The same executable runs as a tool inside each engine's job.
use std::{
    env, fs,
    io::{BufRead, Write},
    net::TcpStream,
    os::unix::{net::UnixStream, process::CommandExt},
    path::PathBuf,
    process::{Command, Stdio},
};
fn run(program: &str, args: &[&str]) {
    assert!(
        Command::new(program).args(args).status().unwrap().success(),
        "{program} {args:?}"
    );
}
fn main() {
    let args: Vec<_> = env::args().skip(1).collect();
    if args.first().is_some_and(|s| s == "outside") {
        fs::write(
            format!(
                "/data/dev/jobs/{}/unexpected-server",
                env::var("PROJ_JOB").unwrap()
            ),
            "started",
        )
        .unwrap();
        return;
    }
    if args.first().is_some_and(|s| s == "github") {
        if args.iter().any(|s| s == "--input") {
            use std::io::Read;
            let mut body = Vec::new();
            std::io::stdin().read_to_end(&mut body).unwrap();
        }
        if args.iter().any(|s| s == "-i" || s == "--include") {
            print!("HTTP/1.1 200 OK\n\n");
        }
        if args.iter().any(|s| s.contains("/comments")) {
            println!("[]");
        } else {
            println!(r#"{{"state":"open","comments":0}}"#);
        }
        return;
    }
    if args.first().is_some_and(|s| s == "drop-heavy") {
        let args: Vec<_> = args[2..]
            .iter()
            .map(|arg| {
                arg.replace("\"/run/burrow-heavy\"=\"read\",", "")
                    .replace(",\"/run/burrow-heavy\"=\"read\"", "")
            })
            .collect();
        panic!(
            "{}",
            Command::new(&env::args().nth(2).unwrap()).args(args).exec()
        );
    }
    if args.first().is_some_and(|s| s == "server") {
        for line in std::io::stdin().lock().lines() {
            let line = line.unwrap();
            // Our clients send integer JSON-RPC ids, and notifications have none.
            let Some(id) = line.split("\"id\":").nth(1) else {
                continue;
            };
            let id = id.trim_start().split([',', '}']).next().unwrap();
            let result = if line.contains("initialize") {
                r#"{"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"reach","version":"1"}}"#
            } else if line.contains("tools/list") {
                r#"{"tools":[{"name":"ping","description":"Check the job's machine MCP","inputSchema":{"type":"object","properties":{}}}]}"#
            } else {
                let job = env::var("PROJ_JOB").unwrap();
                fs::write(format!("/data/dev/jobs/{job}/mcp-called"), "yes").unwrap();
                r#"{"content":[{"type":"text","text":"reachable"}]}"#
            };
            println!("{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}");
        }
        return;
    }
    let job = PathBuf::from(format!("/data/dev/jobs/{}", env::var("PROJ_JOB").unwrap()));
    let scratch = PathBuf::from(env::var("PROJ_SCRATCH").unwrap());
    let declared = env::var("PROJ_CAPABILITIES").unwrap();
    fs::write(job.join("capabilities-declared"), &declared).unwrap();
    let mut passed = Vec::new();
    for capability in declared
        .trim_matches(['[', ']'])
        .split(',')
        .map(|s| s.trim_matches('"'))
    {
        fs::write(job.join("capability-running"), capability).unwrap();
        match capability {
            "workspace" => {
                fs::write("workspace", "ok").unwrap();
                fs::write(scratch.join("scratch"), "ok").unwrap();
                fs::write("/tmp/temporary", "ok").unwrap();
            }
            "job-state" => {
                fs::write(job.join("state-write"), "ok").unwrap();
                run(
                    "proj",
                    &["await", "https://github.com/test/project/issues/1"],
                );
                run("proj", &["ask"]);
            }
            "git-metadata" => {
                let tree = scratch.join("tree");
                run("git", &["fetch", "origin"]);
                run(
                    "git",
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        tree.to_str().unwrap(),
                        "origin/trunk",
                    ],
                );
                run(
                    "git",
                    &[
                        "-C",
                        tree.to_str().unwrap(),
                        "commit",
                        "--allow-empty",
                        "-m",
                        "capabilities: exercise the job contract",
                    ],
                );
                let branch = format!("reach-{}", env::var("PROJ_JOB").unwrap());
                run(
                    "git",
                    &[
                        "-C",
                        tree.to_str().unwrap(),
                        "push",
                        "origin",
                        &format!("HEAD:refs/heads/{branch}"),
                    ],
                );
            }
            "toolchains" => {
                let cargo = env::var("CARGO_HOME")
                    .unwrap_or_else(|_| format!("{}/.cargo", env::var("HOME").unwrap()));
                fs::write(PathBuf::from(cargo).join("job-test"), "ok").unwrap();
            }
            "system" => {
                fs::read_to_string("/etc/os-release").unwrap();
                assert_eq!(
                    fs::read_to_string("/var/lib/job-meters/reach")
                        .unwrap()
                        .trim(),
                    "facts"
                );
                run("git", &["--version"]);
            }
            "github" => {
                UnixStream::connect(env::var("GH_TOKEN_SOCKET").unwrap()).unwrap();
                run(
                    "gh",
                    &[
                        "api",
                        "repos/test/project/issues/1/comments",
                        "-f",
                        "body=capabilities",
                    ],
                );
                run(
                    "gh",
                    &[
                        "api",
                        "repos/test/project/issues/1/labels",
                        "-f",
                        "labels[]=tested",
                    ],
                );
            }
            "heavy" => run("heavy", &["--", "true"]),
            "nix" => {
                assert_eq!(env::var("NIX_REMOTE").unwrap(), "daemon");
                run(
                    "heavy",
                    &[
                        "--",
                        "nix-build",
                        "--no-out-link",
                        "/etc/capability-build.nix",
                    ],
                );
            }
            "kvm" => {
                use std::os::fd::AsRawFd;
                unsafe extern "C" {
                    fn ioctl(fd: i32, request: usize, ...) -> i32;
                }
                let device = fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/dev/kvm")
                    .unwrap();
                assert_eq!(unsafe { ioctl(device.as_raw_fd(), 0xae00) }, 12);
            }
            "mcp" => {
                UnixStream::connect("/run/reach/socket").unwrap();
                let mut child = Command::new(env::current_exe().unwrap())
                    .arg("server")
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .spawn()
                    .unwrap();
                child
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\"}\n")
                    .unwrap();
                assert!(child.wait_with_output().unwrap().status.success());
            }
            "environment" => {
                assert!(!env::var("PROJ_WAIT_DIR").unwrap().is_empty());
                assert!(!env::var("GIT_CONFIG_COUNT").unwrap().is_empty());
                assert!(!env::var("PATH").unwrap().is_empty());
            }
            "network" => {
                TcpStream::connect("127.0.0.1:9418").unwrap();
            }
            other => panic!("capability has no probe: {other}"),
        }
        passed.push(capability);
        fs::write(job.join("capabilities-passed"), passed.join("\n")).unwrap();
    }
    fs::write(job.join("capabilities-passed"), passed.join("\n")).unwrap();
    run("proj", &["done"]);
}
