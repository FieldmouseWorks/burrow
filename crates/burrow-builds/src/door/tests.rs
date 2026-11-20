use std::fs;
use std::io::Cursor;

use super::*;
use crate::tests::{entry, key, secret};
use crate::{Entry, Manifest};

const A: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a";

/// A cn that answers by the token it reads: a public repo's run, a private
/// one's, a refusal or GitHub away. It writes its arguments beside it.
const CN: &str = r#"#!/bin/sh
printf '%s\n' "$@" >"$(dirname "$0")/args"
case "$(cat)" in
  *public) echo '{"repository":"o/r","visibility":"public"}' ;;
  *private) echo '{"repository":"o/p","visibility":"private"}' ;;
  *refused) printf "cn: the job's token doesn't hold:\nits token expired" >&2; exit 65 ;;
  *busy) exit 75 ;;
esac
"#;

struct Door {
    _dir: tempfile::TempDir,
    cfg: Config,
}

fn door() -> Door {
    let dir = tempfile::tempdir().unwrap();
    let cn = dir.path().join("cn");
    burrow_test::executable(&cn, CN);
    let cfg = Config {
        dir: dir.path().join("cache"),
        key: dir.path().join("key"),
        key_name: "test-1".into(),
        max_size: u64::MAX,
        repos: vec!["o/r".into(), "o/p".into()],
        cn: vec![cn.to_string_lossy().into_owned()],
    };
    let k = key();
    fs::write(&cfg.key, format!("{}\n", secret())).unwrap();
    Store {
        dir: cfg.dir.clone(),
        max_size: cfg.max_size,
    }
    .init(&k)
    .unwrap();
    Door { _dir: dir, cfg }
}

/// A request carrying `entries`' manifest and `nars`, with `token`.
fn request(token: Option<&str>, entries: &[Entry], nars: &[u8]) -> (Vec<u8>, String) {
    let manifest = serde_json::to_string(&Manifest {
        paths: entries.to_vec(),
    })
    .unwrap();
    let body = [manifest.as_bytes(), b"\n", nars].concat();
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    let head = format!(
        "POST /upload HTTP/1.0\r\nHost: builds\r\n{auth}Content-Length: {}\r\n\r\n",
        body.len()
    );
    (
        [head.as_bytes(), &body].concat(),
        audience(manifest.as_bytes()),
    )
}

fn ask(d: &Door, req: &[u8]) -> (u16, String) {
    let mut out = Vec::new();
    serve(&d.cfg, &mut Cursor::new(req.to_vec()), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    let status = out[9..12].parse().unwrap();
    (status, out)
}

/// nginx's check of a head carrying `token`, which says how long a body
/// follows and sends none: the status and the whole answer.
fn check(d: &Door, token: Option<&str>) -> (u16, String) {
    let auth = token.map_or(String::new(), |t| format!("Authorization: Bearer {t}\r\n"));
    let req =
        format!("GET /upload HTTP/1.0\r\nHost: builds\r\n{auth}Content-Length: 1073741824\r\n\r\n");
    let mut out = Vec::new();
    serve_admit(&d.cfg, &mut Cursor::new(req.into_bytes()), &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    (out[9..12].parse().unwrap(), out)
}

/// A job's token as GitHub shapes one, its payload claiming `aud`, its
/// signature what the stand-in cn reads.
fn jwt(aud: &str, sig: &str) -> String {
    let payload = json!({ "aud": aud, "repository": "o/r" }).to_string();
    format!("e30.{}.{sig}", URL_SAFE_NO_PAD.encode(payload))
}

fn cn_args(d: &Door) -> Vec<String> {
    fs::read_to_string(d._dir.path().join("args"))
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn a_public_listed_repos_job_writes_and_is_asked_by_its_manifests_audience() {
    let d = door();
    let (req, aud) = request(Some("public"), &[entry(A, b"aaa")], b"aaa");
    let (status, out) = ask(&d, &req);
    assert_eq!(status, 200, "{out}");
    assert!(out.contains(&format!("\"written\":[\"{A}\"]")), "{out}");
    // Conary's rule, asked of cn, with the door's audience and its repos;
    // the token went on stdin, never in its arguments.
    assert_eq!(
        cn_args(&d),
        [
            "attest",
            "job",
            "--audience",
            &aud,
            "--repo",
            "o/r",
            "--repo",
            "o/p"
        ]
    );
    assert!(aud.starts_with("burrow-builds:v1:sha256:"));
}

#[test]
fn whatever_cn_refuses_the_door_refuses_and_writes_nothing() {
    let d = door();
    for (token, status, says) in [
        ("refused", 403, "its token expired"),
        ("private", 403, "o/p is private"),
        ("busy", 503, "try again"),
    ] {
        let (req, _) = request(Some(token), &[entry(A, b"aaa")], b"aaa");
        let (got, out) = ask(&d, &req);
        assert_eq!(got, status, "{token}: {out}");
        assert!(out.contains(says), "{token}: {out}");
    }
    let (req, _) = request(None, &[entry(A, b"aaa")], b"aaa");
    assert_eq!(ask(&d, &req).0, 401);
    let store = Store {
        dir: d.cfg.dir.clone(),
        max_size: 0,
    };
    assert!(!store.has(A));
}

#[test]
fn a_nar_that_isnt_its_manifests_or_a_bad_request_is_refused() {
    let d = door();
    let (req, _) = request(Some("public"), &[entry(A, b"aaa")], b"AAA");
    let (status, out) = ask(&d, &req);
    assert_eq!(status, 422, "{out}");
    let (status, _) = ask(&d, b"GET /upload HTTP/1.0\r\n\r\n");
    assert_eq!(status, 405);
    let (status, _) = ask(&d, b"POST /upload HTTP/1.0\r\n\r\n");
    assert_eq!(status, 411);
    let body = b"{\"paths\":[{\"path\":\"/etc/passwd\"}]}\n";
    let req = [
        format!(
            "POST /upload HTTP/1.0\r\nAuthorization: Bearer public\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .as_bytes(),
        body,
    ]
    .concat();
    assert_eq!(ask(&d, &req).0, 400);
}

#[test]
fn the_check_asks_cn_with_the_audience_the_token_claims_and_reads_no_body() {
    let d = door();
    let aud = audience(b"{\"paths\":[]}");
    let (status, out) = check(&d, Some(&jwt(&aud, "public")));
    assert_eq!(status, 200, "{out}");
    assert!(!out.contains("Burrow-Refused"), "{out}");
    assert_eq!(
        cn_args(&d),
        [
            "attest",
            "job",
            "--audience",
            &aud,
            "--repo",
            "o/r",
            "--repo",
            "o/p"
        ]
    );
}

#[test]
fn the_check_refuses_what_cn_refuses_with_its_reason_in_one_header() {
    let d = door();
    let aud = audience(b"{\"paths\":[]}");
    for (sig, status, says) in [
        ("refused", 403, "its token expired"),
        ("private", 403, "o/p is private"),
        ("busy", 503, "try again"),
    ] {
        let (got, out) = check(&d, Some(&jwt(&aud, sig)));
        assert_eq!(got, status, "{sig}: {out}");
        let header = out
            .lines()
            .find_map(|l| l.strip_prefix("Burrow-Refused: "))
            .unwrap_or_else(|| panic!("{sig}: no Burrow-Refused: {out}"));
        let v: Value = serde_json::from_str(header.trim_end()).unwrap();
        assert!(
            v["refused"].as_str().unwrap().contains(says),
            "{sig}: {header}"
        );
    }
}

#[test]
fn the_check_refuses_no_token_or_one_that_claims_no_uploads_audience_without_cn() {
    let d = door();
    let (status, out) = check(&d, None);
    assert_eq!(status, 401, "{out}");
    assert!(
        out.contains("Burrow-Refused: {\"refused\":\"an upload carries"),
        "{out}"
    );
    let aud = audience(b"{}");
    for token in [
        "public".to_owned(),
        "a.b.public".to_owned(),
        jwt("burrow-builds:v1:sha256:00", "public"),
        jwt(&aud.to_uppercase(), "public"),
        jwt(&format!("{aud} --repo x/y"), "public"),
        format!(
            "e30.{}.public",
            URL_SAFE_NO_PAD.encode(json!({"aud": [aud]}).to_string())
        ),
    ] {
        let (status, out) = check(&d, Some(&token));
        assert_eq!(status, 403, "{token}: {out}");
    }
    assert!(!d._dir.path().join("args").exists(), "cn was asked");
}

#[test]
fn a_long_reason_is_cut_to_fit_nginxs_head() {
    let d = door();
    let cn = d._dir.path().join("cn");
    burrow_test::executable(
        &cn,
        "#!/bin/sh\nyes 'é too long' | head -c 100000 >&2; exit 65\n",
    );
    let (status, out) = check(&d, Some(&jwt(&audience(b"{}"), "x")));
    assert_eq!(status, 403);
    let header = out
        .lines()
        .find(|l| l.starts_with("Burrow-Refused: "))
        .unwrap();
    assert!(
        header.len() <= MOST_REASON + "Burrow-Refused: ".len(),
        "{}",
        header.len()
    );
    assert!(header.contains("too long"), "{header}");
}

/// A client that has gone away: nothing it's sent is read, and nothing a
/// door writes to it lands.
struct Gone;

impl std::io::Write for Gone {
    fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::ErrorKind::BrokenPipe.into())
    }
}

#[test]
fn a_client_gone_before_its_answer_is_not_a_failed_door() {
    // nginx restarts mid-upload when a switch reloads it: the upload is in
    // the store (or refused) before the answer, and the door has no one to
    // tell. Its unit must not fail for it.
    let d = door();
    let (req, _) = request(Some("public"), &[], &[]);
    serve(&d.cfg, &mut Cursor::new(req), &mut Gone).unwrap();
}
