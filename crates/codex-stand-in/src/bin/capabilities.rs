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
    if args.first().is_some_and(|s| s == "listener") {
        let listener = std::os::unix::net::UnixListener::bind("/run/reach/socket").unwrap();
        for stream in listener.incoming() {
            drop(stream.unwrap());
        }
        return;
    }
    if args.first().is_some_and(|s| s == "outside") {
        fs::write("/data/dev/src/web/unexpected-server", "started").unwrap();
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
            Command::new(env::args().nth(2).unwrap()).args(args).exec()
        );
    }
    if args.first().is_some_and(|s| s == "drop-host-jobs") {
        let mut forwarded = Vec::new();
        let mut i = 2;
        while i < args.len() {
            if args[i] == "-c"
                && args.get(i + 1).is_some_and(|s| {
                    s.starts_with("shell_environment_policy.set.PROJ_HOST_JOBS_SOCKET=")
                })
            {
                i += 2;
            } else {
                forwarded.push(args[i].clone());
                i += 1;
            }
        }
        panic!(
            "{}",
            Command::new(&args[1])
                .args(forwarded)
                .env_remove("PROJ_HOST_JOBS_SOCKET")
                .exec()
        );
    }
    if args.first().is_some_and(|s| s == "drop-claude-mcp") {
        let mut forwarded = args[2..].to_vec();
        let index = forwarded
            .iter()
            .position(|arg| arg == "--mcp-config")
            .unwrap();
        forwarded[index + 1] = r#"{"mcpServers":{}}"#.into();
        panic!("{}", Command::new(&args[1]).args(forwarded).exec());
    }
    if args.first().is_some_and(|s| s == "server") {
        for line in std::io::stdin().lock().lines() {
            let line = line.unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let Some(id) = request.get("id") else {
                continue;
            };
            let directory =
                PathBuf::from(format!("/data/dev/jobs/{}", env::var("PROJ_JOB").unwrap()));
            let engine = env::var("MCP_ENGINE").as_deref() == Ok("1");
            if engine {
                assert!(env::var_os("EXTRA_SERVER_ONLY").is_none());
            }
            let response = mcp_reply(&request, &directory, engine);
            println!(
                "{}",
                serde_json::json!({"jsonrpc":"2.0", "id":id, "result":response})
            );
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
                run("proj", &["ask", "--default", "continue the test"]);
                let target = env::var("PROJ_RECOVERY_TARGET").unwrap();
                if fs::read_to_string(job.join("1.engine")).unwrap().trim() == "codex" {
                    assert!(
                        fs::read_dir(format!("/data/dev/jobs/{target}")).is_err(),
                        "the sandbox must use the broker, not host job paths"
                    );
                }
                let history = Command::new("proj")
                    .args(["log", &target, "--turn", "1", "--raw"])
                    .output()
                    .unwrap();
                assert!(
                    history.status.success(),
                    "{}",
                    String::from_utf8_lossy(&history.stderr)
                );
                assert!(
                    String::from_utf8_lossy(&history.stdout).contains("host recovery transcript")
                );
                run("proj", &["reply", &target, "continue after timeout"]);
                let until = std::time::Instant::now() + std::time::Duration::from_secs(90);
                loop {
                    assert!(
                        std::time::Instant::now() < until,
                        "target did not finish its resumed turn"
                    );
                    let state = Command::new("proj")
                        .args(["state", &target])
                        .output()
                        .unwrap();
                    assert!(state.status.success());
                    match String::from_utf8_lossy(&state.stdout).trim() {
                        "queued" | "running" => {
                            std::thread::sleep(std::time::Duration::from_millis(100))
                        }
                        "done" => break,
                        other => panic!("resumed target failed: {other}"),
                    }
                }
                let answer = Command::new("proj")
                    .args(["log", &target])
                    .output()
                    .unwrap();
                assert!(
                    answer.status.success(),
                    "{}",
                    String::from_utf8_lossy(&answer.stderr)
                );
                assert!(String::from_utf8_lossy(&answer.stdout).starts_with("done\n"));
                run("proj", &["stop", &target]);
                fs::write(job.join("host-job-recovered"), "ok").unwrap();
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
            "probe" => {
                let answer = Command::new("proj")
                    .args(["probe", "deployed"])
                    .output()
                    .unwrap();
                assert!(
                    answer.status.success(),
                    "{}",
                    String::from_utf8_lossy(&answer.stderr)
                );
                assert!(!answer.stdout.is_empty());
                fs::write(job.join("host-probe"), answer.stdout).unwrap();
            }
            "github" => {
                UnixStream::connect(env::var("GH_TOKEN_SOCKET").unwrap()).unwrap();
                let mut github = burrow_github::Gh::default();
                for (path, body) in [
                    (
                        "repos/test/project/issues/1/comments",
                        serde_json::json!({"body":"capabilities"}),
                    ),
                    (
                        "repos/test/project/issues/1/labels",
                        serde_json::json!({"labels":["tested"]}),
                    ),
                ] {
                    burrow_github::call(&mut github, "POST", path, Some(&body), None).unwrap();
                }
            }
            "heavy" => run("heavy", &["--", "true"]),
            "nix" => {
                assert_eq!(env::var("NIX_REMOTE").unwrap(), "daemon");
                let id = env::var("PROJ_JOB").unwrap();
                run(
                    "heavy",
                    &[
                        "--",
                        "nix-build",
                        "--no-out-link",
                        "/etc/capability-build.nix",
                        "--argstr",
                        "job",
                        &id,
                    ],
                );
            }
            "kvm" => {
                #[cfg(target_os = "linux")]
                {
                    let device = kvm_ioctls::Kvm::new().unwrap();
                    assert_eq!(device.get_api_version(), 12);
                    let _vm = device.create_vm().unwrap();
                }
                #[cfg(not(target_os = "linux"))]
                panic!("KVM capabilities require Linux");
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
                    .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"ping\"}}\n")
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

fn mcp_reply(
    request: &serde_json::Value,
    directory: &std::path::Path,
    engine: bool,
) -> serde_json::Value {
    use serde_json::json;
    match request["method"].as_str() {
        Some("initialize") => {
            json!({"protocolVersion":"2024-11-05", "capabilities":{"tools":{}}, "serverInfo":{"name":"reach","version":"1"}})
        }
        Some("tools/list") => {
            json!({"tools":[{"name":"ping","description":"Check the job's machine MCP","inputSchema":{"type":"object","properties":{}}}]})
        }
        Some("tools/call") if request["params"]["name"] == "ping" => {
            fs::write(
                directory.join(if engine { "mcp-called" } else { "mcp-direct" }),
                "yes",
            )
            .unwrap();
            json!({"content":[{"type":"text","text":"reachable"}]})
        }
        _ => json!({}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn only_the_engine_tool_call_can_create_its_success_marker() {
        let directory = env::temp_dir().join(format!("mcp-evidence-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        for method in ["initialize", "tools/list", "ping", "resources/list"] {
            mcp_reply(&json!({"method":method}), &directory, true);
            assert!(!directory.join("mcp-called").exists());
        }
        let call = json!({"method":"tools/call", "params":{"name":"ping"}});
        mcp_reply(&call, &directory, false);
        assert!(directory.join("mcp-direct").exists());
        assert!(!directory.join("mcp-called").exists());
        mcp_reply(&call, &directory, true);
        assert!(directory.join("mcp-called").exists());
        fs::remove_dir_all(directory).unwrap();
    }
}
