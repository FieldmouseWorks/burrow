//! robot as its wrapper runs it: the config built in, the credentials and
//! the server from the environment, against a stand-in webservice
//! (common/mod.rs).

mod common;

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

use common::Webservice;

const IP: &str = "203.0.113.7";

/// What Robot answers for the example server.
const SERVER: &str = r#"{"server":{"server_ip":"203.0.113.7","server_ipv6_net":"2001:db8::","server_number":321,"server_name":"box","product":"AX41","dc":"FSN1-DC14","traffic":"unlimited","status":"ready","cancelled":false}}"#;
const RESCUE: &str = r#"{"rescue":{"server_ip":"203.0.113.7","os":["linux","vkvm"],"active":false,"password":null,"authorized_key":[],"host_key":[]}}"#;
const RESET: &str = r#"{"reset":{"server_ip":"203.0.113.7","type":["sw","hw","man"],"operating_status":"not supported"}}"#;

struct Box {
    _dir: tempfile::TempDir,
    config: PathBuf,
    creds: PathBuf,
}

impl Box {
    fn new() -> Box {
        let dir = tempfile::tempdir().unwrap();
        let creds = dir.path().join("robot");
        fs::write(&creds, "cfg:pw\n").unwrap();
        let config = dir.path().join("robot.json");
        fs::write(
            &config,
            serde_json::json!({ "serverIp": IP, "credentialsFile": creds }).to_string(),
        )
        .unwrap();
        Box {
            _dir: dir,
            config,
            creds,
        }
    }

    /// robot with these words, asking `url`, with nothing of the caller's
    /// ROBOT_ variables or proxies.
    fn robot(&self, url: &str, args: &[&str]) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_burrow-robot"));
        cmd.arg(&self.config).args(args).env("ROBOT_URL", url);
        for var in [
            "ROBOT_USER",
            "ROBOT_PASSWORD",
            "ROBOT_CREDS_FILE",
            "ROBOT_SERVER_IP",
            "ALL_PROXY",
            "all_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "HTTP_PROXY",
            "http_proxy",
        ] {
            cmd.env_remove(var);
        }
        cmd
    }

    fn run(&self, url: &str, args: &[&str]) -> Output {
        self.robot(url, args).output().unwrap()
    }
}

fn said(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn status_shows_the_server_its_rescue_and_its_reset() {
    let ws = Webservice::serve(&[
        ("GET /server/203.0.113.7", 200, SERVER),
        ("GET /boot/203.0.113.7/rescue", 200, RESCUE),
        ("GET /reset/203.0.113.7", 200, RESET),
    ]);
    let b = Box::new();
    for args in [&[][..], &["status"]] {
        let out = b.run(&ws.url, args);
        assert!(out.status.success(), "{:?}", said(&out));
        assert_eq!(
            said(&out),
            (
                r#"{
  "server_name": "box",
  "server_ip": "203.0.113.7",
  "product": "AX41",
  "dc": "FSN1-DC14",
  "status": "ready"
}
{
  "active": false,
  "os": [
    "linux",
    "vkvm"
  ],
  "authorized_key": []
}
{
  "type": [
    "sw",
    "hw",
    "man"
  ],
  "operating_status": "not supported"
}
"#
                .to_owned(),
                String::new()
            )
        );
    }
    assert_eq!(
        ws.calls(),
        [
            "GET /server/203.0.113.7",
            "GET /boot/203.0.113.7/rescue",
            "GET /reset/203.0.113.7"
        ]
        .repeat(2)
    );
    for asked in ws.asked() {
        assert_eq!(asked.authorization, "Basic Y2ZnOnB3", "{asked:?}");
        assert_eq!(asked.body, "");
    }
}

#[test]
fn rescue_on_arms_every_key_robot_has() {
    let ws = Webservice::serve(&[
        (
            "GET /key",
            200,
            r#"[{"key":{"name":"laptop","fingerprint":"aa:bb"}},{"key":{"name":"remi","fingerprint":"cc:dd"}}]"#,
        ),
        (
            "POST /boot/203.0.113.7/rescue",
            200,
            r#"{"rescue":{"server_ip":"203.0.113.7","os":"linux","active":true,"password":"Rescue1","authorized_key":["aa:bb","cc:dd"]}}"#,
        ),
    ]);
    let out = Box::new().run(&ws.url, &["rescue", "on"]);
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(
        said(&out),
        (
            "{\n  \"active\": true,\n  \"os\": \"linux\",\n  \"password\": \"Rescue1\"\n}\n"
                .to_owned(),
            "rescue armed: it boots on the next reset (robot reset)\n".to_owned()
        )
    );
    assert_eq!(ws.calls(), ["GET /key", "POST /boot/203.0.113.7/rescue"]);
    let armed = &ws.asked()[1];
    assert_eq!(armed.content_type, "application/x-www-form-urlencoded");
    assert_eq!(
        armed.body,
        "os=linux&authorized_key[]=aa%3Abb&authorized_key[]=cc%3Add"
    );
}

#[test]
fn rescue_on_with_a_key_arms_only_that_one() {
    let ws = Webservice::serve(&[(
        "POST /boot/203.0.113.7/rescue",
        200,
        r#"{"rescue":{"os":"linux","active":true,"password":"Rescue2"}}"#,
    )]);
    let out = Box::new().run(&ws.url, &["rescue", "on", "ee:ff"]);
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(ws.calls(), ["POST /boot/203.0.113.7/rescue"]);
    assert_eq!(ws.asked()[0].body, "os=linux&authorized_key[]=ee%3Aff");
    // Robot with no keys at all: the rescue system's password only.
    let ws = Webservice::serve(&[
        ("GET /key", 200, "[]"),
        (
            "POST /boot/203.0.113.7/rescue",
            200,
            r#"{"rescue":{"os":"linux","active":true,"password":"Rescue3"}}"#,
        ),
    ]);
    assert!(Box::new().run(&ws.url, &["rescue", "on"]).status.success());
    assert_eq!(ws.asked()[1].body, "os=linux");
}

#[test]
fn rescue_off_cancels_it() {
    let ws = Webservice::serve(&[(
        "DELETE /boot/203.0.113.7/rescue",
        200,
        r#"{"rescue":{"server_ip":"203.0.113.7","os":["linux"],"active":false,"password":null}}"#,
    )]);
    let out = Box::new().run(&ws.url, &["rescue", "off"]);
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(said(&out).0, "{\n  \"active\": false\n}\n");
    assert_eq!(ws.calls(), ["DELETE /boot/203.0.113.7/rescue"]);
}

#[test]
fn reset_is_hw_unless_it_says_otherwise() {
    let ws = Webservice::serve(&[(
        "POST /reset/203.0.113.7",
        200,
        r#"{"reset":{"server_ip":"203.0.113.7","type":"hw"}}"#,
    )]);
    let b = Box::new();
    let out = b.run(&ws.url, &["reset"]);
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(
        said(&out).0,
        "{\n  \"server_ip\": \"203.0.113.7\",\n  \"type\": \"hw\"\n}\n"
    );
    for kind in ["sw", "power"] {
        assert!(b.run(&ws.url, &["reset", kind]).status.success());
    }
    let bodies: Vec<String> = ws.asked().into_iter().map(|a| a.body).collect();
    assert_eq!(bodies, ["type=hw", "type=sw", "type=power"]);
    assert!(
        ws.asked()
            .iter()
            .all(|a| a.content_type == "application/x-www-form-urlencoded")
    );
}

#[test]
fn wol_wakes_it() {
    let ws = Webservice::serve(&[(
        "POST /wol/203.0.113.7",
        200,
        r#"{"wol":{"server_ip":"203.0.113.7"}}"#,
    )]);
    let out = Box::new().run(&ws.url, &["wol"]);
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(said(&out).0, "{\n  \"server_ip\": \"203.0.113.7\"\n}\n");
    assert_eq!(ws.calls(), ["POST /wol/203.0.113.7"]);
    assert_eq!(ws.asked()[0].body, "");
}

#[test]
fn the_server_and_the_credentials_can_come_from_the_environment() {
    let ws = Webservice::serve(&[("POST /wol/198.51.100.9", 200, r#"{"wol":{}}"#)]);
    let b = Box::new();
    let out = b
        .robot(&format!("{}/", ws.url), &["wol"])
        .env("ROBOT_SERVER_IP", "198.51.100.9")
        .env("ROBOT_USER", "robot-user")
        .env("ROBOT_PASSWORD", "p@ss \"word\"")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(said(&out).0, "{}\n");
    assert_eq!(
        ws.calls(),
        ["POST /wol/198.51.100.9"],
        "a trailing slash on the URL"
    );
    assert_eq!(
        ws.asked()[0].authorization,
        "Basic cm9ib3QtdXNlcjpwQHNzICJ3b3JkIg=="
    );
    // ROBOT_CREDS_FILE, its newline dropped; an empty ROBOT_USER is unset.
    let other = b.creds.with_file_name("other");
    fs::write(&other, "u:from file\n").unwrap();
    let ws = Webservice::serve(&[("POST /wol/203.0.113.7", 200, r#"{"wol":{}}"#)]);
    let out = b
        .robot(&ws.url, &["wol"])
        .env("ROBOT_CREDS_FILE", &other)
        .env("ROBOT_USER", "")
        .output()
        .unwrap();
    assert!(out.status.success(), "{:?}", said(&out));
    assert_eq!(ws.asked()[0].authorization, "Basic dTpmcm9tIGZpbGU=");
}

#[test]
fn a_refusal_says_robots_reason_after_what_it_got() {
    let ws = Webservice::serve(&[
        ("GET /server/203.0.113.7", 200, SERVER),
        ("GET /boot/203.0.113.7/rescue", 200, RESCUE),
        (
            "GET /reset/203.0.113.7",
            401,
            r#"{"error":{"status":401,"code":"UNAUTHORIZED","message":"Unauthorized"}}"#,
        ),
    ]);
    let out = Box::new().run(&ws.url, &[]);
    assert_eq!(out.status.code(), Some(1));
    let (stdout, stderr) = said(&out);
    assert_eq!(stdout.matches("\n}\n").count(), 2, "{stdout}");
    assert_eq!(
        stderr,
        "robot: GET /reset/203.0.113.7: HTTP 401 UNAUTHORIZED: Unauthorized\n"
    );
    // A server it doesn't know, and an answer that isn't Robot's.
    let ws = Webservice::serve(&[("POST /wol/203.0.113.7", 502, "<html>bad gateway</html>")]);
    let b = Box::new();
    let out = b.run(&ws.url, &["rescue", "off"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        said(&out).1,
        "robot: DELETE /boot/203.0.113.7/rescue: HTTP 404 NOT_FOUND: Not found\n"
    );
    let out = b.run(&ws.url, &["wol"]);
    assert_eq!(
        said(&out),
        (
            String::new(),
            "robot: POST /wol/203.0.113.7: HTTP 502\n".to_owned()
        )
    );
}

/// curl followed no redirect, and robot follows none: the 3xx is the answer,
/// a refusal, and nothing else is asked, so a POST is never sent on as a GET
/// or to another host.
#[test]
fn a_redirect_is_a_refusal_and_not_followed() {
    for status in [301, 302, 303, 307, 308] {
        let ws = Webservice::serve(&[("POST /reset/203.0.113.7", status, "")]);
        let out = Box::new().run(&ws.url, &["reset"]);
        assert_eq!(out.status.code(), Some(1), "{status}");
        assert_eq!(
            said(&out).1,
            format!("robot: POST /reset/203.0.113.7: HTTP {status}\n")
        );
        assert_eq!(ws.calls(), ["POST /reset/203.0.113.7"], "{status}");
    }
}

#[test]
fn a_webservice_it_cant_reach_is_said_without_the_credentials() {
    let b = Box::new();
    let out = b.run(&common::nobody(), &["reset"]);
    assert_eq!(out.status.code(), Some(1));
    let (stdout, stderr) = said(&out);
    assert_eq!(stdout, "");
    assert!(
        stderr.starts_with("robot: POST /reset/203.0.113.7: "),
        "{stderr}"
    );
    assert!(
        !stderr.contains("pw") && !stderr.contains("Y2ZnOnB3"),
        "{stderr}"
    );
}

#[test]
fn without_credentials_it_asks_nothing() {
    let ws = Webservice::serve(&[]);
    let b = Box::new();
    fs::remove_file(&b.creds).unwrap();
    let out = b.run(&ws.url, &["status"]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        said(&out).1,
        format!(
            "robot: no Robot credentials: put user:password in {}\n",
            b.creds.display()
        )
    );
    assert!(ws.calls().is_empty());
}

#[test]
fn what_it_cant_run() {
    let ws = Webservice::serve(&[]);
    let b = Box::new();
    for (args, said_) in [
        (&["reset", "cold"][..], "reset type: sw, hw or power\n"),
        (&["rescue"], "usage: robot rescue on [KEY_FP] | off\n"),
        (
            &["cancel"],
            "usage: robot status | rescue on [KEY_FP] | rescue off | reset [sw|hw|power] | wol\n",
        ),
    ] {
        let out = b.run(&ws.url, args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(said(&out), (String::new(), said_.to_owned()), "{args:?}");
    }
    let bin = env!("CARGO_BIN_EXE_burrow-robot");
    assert_eq!(Command::new(bin).output().unwrap().status.code(), Some(2));
    let extra = b.config.with_file_name("extra.json");
    fs::write(
        &extra,
        serde_json::json!({ "serverIp": IP, "credentialsFile": b.creds, "url": "x" }).to_string(),
    )
    .unwrap();
    let out = Command::new(bin).arg(&extra).arg("wol").output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        said(&out).1.contains("unknown field `url`"),
        "{:?}",
        said(&out)
    );
    assert!(ws.calls().is_empty());
}

/// /dev/full: every write to it fails, as a full disk's does.
fn full() -> std::process::Stdio {
    fs::OpenOptions::new()
        .write(true)
        .open("/dev/full")
        .unwrap()
        .into()
}

fn armed() -> Webservice {
    Webservice::serve(&[(
        "POST /boot/203.0.113.7/rescue",
        200,
        r#"{"rescue":{"os":"linux","active":true,"password":"Rescue1"}}"#,
    )])
}

#[test]
fn a_rescue_password_it_cant_write_fails_and_arms_only_once() {
    let ws = armed();
    let out = Box::new()
        .robot(&ws.url, &["rescue", "on", "ee:ff"])
        .stdout(full())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", said(&out));
    let (_, err) = said(&out);
    assert!(err.contains("rescue armed"), "{err}");
    assert!(err.contains("robot: stdout: "), "{err}");
    assert_eq!(ws.calls(), ["POST /boot/203.0.113.7/rescue"]);
}

#[test]
fn a_notice_it_cant_write_fails_with_the_password_still_out() {
    let ws = armed();
    let out = Box::new()
        .robot(&ws.url, &["rescue", "on", "ee:ff"])
        .stderr(full())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", said(&out));
    assert!(said(&out).0.contains("Rescue1"), "{:?}", said(&out));
    assert_eq!(ws.calls(), ["POST /boot/203.0.113.7/rescue"]);
}

#[test]
fn every_call_fails_on_output_it_cant_write() {
    let answers: &[(&[&str], &str, &str)] = &[
        (
            &["rescue", "off"],
            "DELETE /boot/203.0.113.7/rescue",
            r#"{"rescue":{"active":false}}"#,
        ),
        (
            &["reset"],
            "POST /reset/203.0.113.7",
            r#"{"reset":{"type":"hw"}}"#,
        ),
        (
            &["wol"],
            "POST /wol/203.0.113.7",
            r#"{"wol":{"server_ip":"203.0.113.7"}}"#,
        ),
        (&["status"], "GET /server/203.0.113.7", r#"{"server":{}}"#),
    ];
    for (args, call, body) in answers {
        let ws = if *args == ["status"] {
            Webservice::serve(&[
                (call, 200, body),
                ("GET /boot/203.0.113.7/rescue", 200, RESCUE),
                ("GET /reset/203.0.113.7", 200, RESET),
            ])
        } else {
            Webservice::serve(&[(call, 200, body)])
        };
        let out = Box::new()
            .robot(&ws.url, args)
            .stdout(full())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{args:?}: {:?}", said(&out));
        assert!(
            said(&out).1.contains("robot: stdout: "),
            "{args:?}: {:?}",
            said(&out)
        );
        // Nothing is asked again to make up for it.
        assert_eq!(
            ws.calls().len(),
            if *args == ["status"] { 3 } else { 1 },
            "{args:?}"
        );
    }
}

#[test]
fn a_refusal_is_still_said_when_the_output_cant_be_written() {
    let ws = Webservice::serve(&[
        ("GET /server/203.0.113.7", 200, SERVER),
        ("GET /boot/203.0.113.7/rescue", 500, "{}"),
    ]);
    let out = Box::new()
        .robot(&ws.url, &["status"])
        .stdout(full())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{:?}", said(&out));
    let (_, err) = said(&out);
    assert!(err.contains("robot: stdout: "), "{err}");
    assert!(
        err.contains("robot: GET /boot/203.0.113.7/rescue: "),
        "{err}"
    );
}

#[test]
fn nowhere_to_say_anything_is_a_failure_and_no_panic() {
    let ws = armed();
    let out = Box::new()
        .robot(&ws.url, &["rescue", "on", "ee:ff"])
        .stdout(full())
        .stderr(full())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    // Usage keeps its own exit code.
    let out = Box::new()
        .robot("http://127.0.0.1:9", &["reset", "cold"])
        .stderr(full())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    // A refusal with nowhere to say it.
    let ws = Webservice::serve(&[("POST /wol/203.0.113.7", 500, "{}")]);
    let out = Box::new()
        .robot(&ws.url, &["wol"])
        .stderr(full())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}
