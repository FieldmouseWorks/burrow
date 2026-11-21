use super::*;
use tempfile::TempDir;

const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn me() -> u32 {
    rustix::process::getuid().as_raw()
}

/// A door that does `body` with REPO PR HEAD as $1 $2 $3. It finds its
/// tools on the test's PATH, set in the shell and never exported, since the
/// broker gives it none.
fn door(dir: &Path, body: &str) -> Vec<String> {
    let path = dir.join("door");
    let tools = std::env::var("PATH").unwrap_or_default();
    burrow_test::executable(&path, format!("#!/bin/sh\nPATH='{tools}'\n{body}\n"));
    vec![path.display().to_string()]
}

/// A broker for `askers` on `door`, serving at the socket it returns.
fn broker(dir: &Path, askers: Vec<u32>, door: Vec<String>) -> (Arc<Broker>, PathBuf) {
    let socket = dir.join("door.sock");
    let listener = bind(&socket).unwrap();
    let broker = Arc::new(Broker::bare(askers, door));
    let serving = broker.clone();
    thread::spawn(move || serving.serve(&listener));
    (broker, socket)
}

/// Wait for `path`, failing the test past a deadline rather than hanging it.
fn wait_for(path: &Path) {
    let until = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(Instant::now() < until, "{} never came", path.display());
        thread::sleep(Duration::from_millis(20));
    }
}

fn request(pr: u64) -> Request {
    Request {
        repo: "Sam/R".into(),
        pr,
        head: HEAD.into(),
    }
}

#[test]
fn an_asker_gets_what_the_door_said_and_its_exit() {
    let dir = TempDir::new().unwrap();
    let body = r#"echo "to stderr" >&2
printf '{"outcome": "declined", "why": "%s %s %s, home %s, token %s"}\n' "$1" "$2" "$3" "$HOME" "${GH_TOKEN:-none}"
exit 2"#;
    let (_, socket) = broker(dir.path(), vec![me()], door(dir.path(), body));
    let reply = ask(&socket, &request(5), WAIT);
    assert_eq!(reply.code, 2, "{reply:?}");
    let home = std::env::var("HOME").unwrap_or_default();
    assert_eq!(
        reply.said,
        format!(r#"{{"outcome": "declined", "why": "Sam/R 5 {HEAD}, home {home}, token none"}}"#)
    );
}

#[test]
fn the_door_runs_with_none_of_the_brokers_environment_but_its_home() {
    let dir = TempDir::new().unwrap();
    let (_, socket) = broker(
        dir.path(),
        vec![me()],
        door(dir.path(), "env | sort | tr '\\n' ' '"),
    );
    let reply = ask(&socket, &request(5), WAIT);
    assert_eq!(reply.code, 0, "{reply:?}");
    let names: Vec<&str> = reply
        .said
        .split_whitespace()
        .filter_map(|v| v.split_once('=').map(|(n, _)| n))
        .filter(|n| !["PWD", "SHLVL", "_", "OLDPWD"].contains(n))
        .collect();
    assert!(names.iter().all(|n| *n == "HOME"), "{}", reply.said);
}

#[test]
fn anyone_else_is_refused_and_the_door_never_runs() {
    let dir = TempDir::new().unwrap();
    let ran = dir.path().join("ran");
    let body = format!("touch {}", ran.display());
    let (_, socket) = broker(
        dir.path(),
        vec![me().wrapping_add(1)],
        door(dir.path(), &body),
    );
    let reply = ask(&socket, &request(5), WAIT);
    assert_eq!(reply.code, 1, "{reply:?}");
    let said: serde_json::Value = serde_json::from_str(&reply.said).unwrap();
    assert_eq!(said["outcome"], "failed");
    assert_eq!(
        said["why"],
        format!("the owner's door's broker: uid {} may not ask", me())
    );
    assert!(!ran.exists());
}

#[test]
fn a_second_ask_of_a_head_whose_door_runs_is_refused() {
    let dir = TempDir::new().unwrap();
    let go = dir.path().join("go");
    let started = dir.path().join("started");
    let body = format!(
        "touch {}\nwhile [ ! -e {} ]; do sleep 0.05; done\necho '{{\"outcome\": \"posted\", \"at\": 1}}'",
        started.display(),
        go.display()
    );
    let (_, socket) = broker(dir.path(), vec![me()], door(dir.path(), &body));
    let first = {
        let socket = socket.clone();
        thread::spawn(move || ask(&socket, &request(5), WAIT))
    };
    wait_for(&started);
    // The same head, its repo in another case: refused.
    let mut again = request(5);
    again.repo = "sam/r".into();
    let second = ask(&socket, &again, WAIT);
    assert_eq!(second.code, 1, "{second:?}");
    assert!(second.said.contains("already asking"), "{}", second.said);
    std::fs::write(&go, "").unwrap();
    let first = first.join().unwrap();
    assert_eq!(
        (first.code, first.said.as_str()),
        (0, r#"{"outcome": "posted", "at": 1}"#)
    );
    // Once it's done, the head may be asked again.
    assert_eq!(ask(&socket, &request(5), WAIT).code, 0);
}

#[test]
fn the_door_is_stopped_when_its_asker_goes() {
    let dir = TempDir::new().unwrap();
    let pid = dir.path().join("pid");
    let posted = dir.path().join("posted");
    let body = format!(
        "echo $$ > {}.new && mv {0}.new {0}\nsleep 3\ntouch {}",
        pid.display(),
        posted.display()
    );
    let (_, socket) = broker(dir.path(), vec![me()], door(dir.path(), &body));
    let stream = UnixStream::connect(&socket).unwrap();
    let mut line = serde_json::to_vec(&request(5)).unwrap();
    line.push(b'\n');
    (&stream).write_all(&line).unwrap();
    wait_for(&pid);
    drop(stream);
    thread::sleep(Duration::from_secs(4));
    assert!(!posted.exists(), "a door its asker gave up on went on");
}

#[test]
fn a_door_past_its_limit_is_stopped_and_fails() {
    let dir = TempDir::new().unwrap();
    let posted = dir.path().join("posted");
    let body = format!("sleep 3\ntouch {}", posted.display());
    let socket = dir.path().join("door.sock");
    let listener = bind(&socket).unwrap();
    let mut broker = Broker::bare(vec![me()], door(dir.path(), &body));
    broker.limit = Duration::from_millis(300);
    thread::spawn(move || Arc::new(broker).serve(&listener));
    let reply = ask(&socket, &request(5), WAIT);
    assert_eq!(reply.code, 1, "{reply:?}");
    assert!(reply.said.contains("time limit"), "{}", reply.said);
    thread::sleep(Duration::from_secs(4));
    assert!(!posted.exists());
}

#[test]
fn no_broker_is_a_failure_the_asker_reads() {
    let dir = TempDir::new().unwrap();
    let reply = ask(&dir.path().join("none.sock"), &request(5), WAIT);
    assert_eq!(reply.code, 1);
    let said: serde_json::Value = serde_json::from_str(&reply.said).unwrap();
    assert_eq!(said["outcome"], "failed");
}

#[test]
fn a_user_is_found_by_name() {
    let passwd = "root:x:0:0::/root:/bin/sh\ndev:x:1000:100::/home/dev:/bin/sh\n";
    assert_eq!(uid(passwd, "dev"), Ok(1000));
    assert!(uid(passwd, "de").is_err());
}
