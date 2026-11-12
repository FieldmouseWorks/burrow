//! Host-side GitHub transport. Only responses fetched here can update the
//! installation meter; clients cannot submit response evidence or identities.
use crate::{answer, target::Target};
use base64::{Engine, engine::general_purpose::STANDARD};
use burrow_github::{Response, quota::Meter};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    path::PathBuf,
    time::Duration,
};

pub struct Config {
    pub app: String,
    pub meters: PathBuf,
    pub kick: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    owner: String,
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}
#[derive(Serialize)]
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}
fn io_error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}
fn agent(config: &answer::Config) -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(config.proxy.clone())
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(30)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .into()
}
fn fetch(
    agent: &ureq::Agent,
    api: &str,
    token: &str,
    r: &Request,
) -> io::Result<(Response, Vec<u8>)> {
    // Path and query only: never let client input choose a host or supply a
    // fragment, userinfo, or a second authority. Redirects return to gh.
    if !r.path.starts_with('/')
        || r.path.starts_with("//")
        || r.path.contains(['#', '\\', '\r', '\n'])
    {
        return Err(io_error("invalid GitHub API path"));
    }
    let mut request = ureq::http::Request::builder()
        .method(r.method.as_str())
        .uri(format!("{}{}", api.trim_end_matches('/'), r.path));
    for (key, value) in &r.headers {
        if ![
            "authorization",
            "host",
            "content-length",
            "connection",
            "transfer-encoding",
        ]
        .iter()
        .any(|s| key.eq_ignore_ascii_case(s))
        {
            request = request.header(key, value);
        }
    }
    let body = STANDARD.decode(&r.body).map_err(io_error)?;
    let request = request
        .header("Authorization", format!("Bearer {token}"))
        .body(body)
        .map_err(io_error)?;
    let mut response = agent.run(request).map_err(io_error)?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.to_string(), v.to_owned())))
        .collect();
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err(io_error("GitHub response exceeds transport limit"));
    }
    let response = Response {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).into_owned(),
    };
    Ok((response, bytes))
}
fn publish(config: &Config, meter: &Meter) -> io::Result<()> {
    std::fs::create_dir_all(&config.meters)?;
    meter.record_public(&config.meters)?;
    Ok(())
}
pub fn answer(
    config: &answer::Config,
    quota: &Config,
    input: impl Read,
    mut output: impl Write,
    now: i64,
) -> io::Result<()> {
    let request: Request =
        serde_json::from_reader(input.take(96 * 1024 * 1024)).map_err(io_error)?;
    let target = Target::parse(&request.owner)
        .filter(|t| t.repo.is_none())
        .ok_or_else(|| io_error("an installation owner is required"))?;
    let token = answer::token(config, &target.owner, now).map_err(io_error)?;
    let agent = agent(config);
    let (mut response, bytes) = fetch(&agent, &config.api, &token, &request)?;
    // REST success data cannot become GraphQL errors, including raw content
    // whose filename happens to end in graphql. No redirect is followed here.
    let path = request.path.split('?').next().unwrap_or_default();
    if path != "/graphql" && path != "/api/graphql" && response.status < 400 {
        response.body.clear();
    }
    let mut meter = Meter::new(&quota.app, &target.owner, now.max(0) as u64);
    meter.remaining = response
        .header("x-ratelimit-remaining")
        .and_then(|s| s.parse().ok());
    meter.reset = response
        .header("x-ratelimit-reset")
        .and_then(|s| s.parse().ok());
    meter.response(&response);
    // Every persisted extension is published, before and after a free probe.
    // State/probe failures must not turn a completed mutation into a retry.
    if let Err(e) = publish(quota, &meter) {
        eprintln!("gh-token: quota state: {e}");
    }
    // Complete the caller's response before optional observation I/O. A slow
    // free probe cannot turn an already completed mutation into a timeout.
    let delivered = serde_json::to_writer(
        &mut output,
        &Reply {
            status: response.status,
            headers: response.headers.clone(),
            body: STANDARD.encode(&bytes),
        },
    )
    .map_err(io_error)
    .and_then(|()| output.flush());
    if let Some(url) = &quota.kick {
        let _ = burrow_events::kick::kick(url, Duration::from_secs(1));
    }
    let probe = Request {
        owner: target.owner,
        method: "GET".into(),
        path: "/rate_limit".into(),
        headers: vec![],
        body: String::new(),
    };
    if let Ok((budget, _)) = fetch(&agent, &config.api, &token, &probe)
        && budget.status == 200
        && let Ok(value) = serde_json::from_str(&budget.body)
    {
        meter.at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        meter.budget(&value);
        if let Err(e) = publish(quota, &meter) {
            eprintln!("gh-token: quota state: {e}");
        }
        if let Some(url) = &quota.kick {
            let _ = burrow_events::kick::kick(url, Duration::from_secs(1));
        }
    }
    delivered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;
    use serde_json::json;
    #[test]
    fn only_host_fetched_responses_authorize_the_selected_installation() {
        let server = fake::serve(|r| {
            if !r.path.starts_with("/app/") {
                assert_eq!(r.auth, "Bearer fixture");
            }
            if r.path == "/app/installations?per_page=100&page=1" {
                return (200, vec![], fake::installations(&[(42, "sam")]));
            }
            if r.path.ends_with("/access_tokens") {
                return (
                    201,
                    vec![],
                    r#"{"token":"fixture","expires_at":"2099-01-01T00:00:00Z"}"#.into(),
                );
            }
            if r.path == "/rate_limit" {
                return (
                    200,
                    vec![],
                    r#"{"resources":{"core":{"remaining":123,"reset":4000000000}}}"#.into(),
                );
            }
            match r.path.as_str() {
                "/limited" => (403, vec![("retry-after", "120".into())], "{}".into()),
                "/denied" => (
                    403,
                    vec![],
                    r#"{"message":"Resource not accessible by integration"}"#.into(),
                ),
                "/redirect/graphql" => (
                    302,
                    vec![("location", "https://example.invalid/graphql".into())],
                    r#"{"errors":[{"type":"RATE_LIMITED"}]}"#.into(),
                ),
                _ => (
                    200,
                    vec![],
                    r#"{"errors":[{"type":"RATE_LIMITED"}]}"#.into(),
                ),
            }
        });
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("key"), fake::key(true)).unwrap();
        std::fs::create_dir(tmp.path().join("cache")).unwrap();
        let config = answer::Config {
            api: server.url.clone(),
            proxy: None,
            app_id: 7,
            key: tmp.path().join("key"),
            cache: tmp.path().join("cache"),
        };
        for (path, limited, status) in [
            ("/repos/o/r/contents/graphql", false, 200),
            ("/unrelated/graphql", false, 200),
            ("/redirect/graphql", false, 302),
            ("/denied", false, 403),
            ("/graphql", true, 200),
            ("/limited", true, 403),
        ] {
            let quota = Config {
                app: "actual-app".into(),
                meters: tmp.path().join(path.trim_start_matches('/')),
                kick: None,
            };
            let mut out = Vec::new();
            let request = json!({"owner":"sam","method":"GET","path":path,"headers":[["Authorization","Bearer forged"]],"body":""});
            answer(
                &config,
                &quota,
                request.to_string().as_bytes(),
                &mut out,
                1793224800,
            )
            .unwrap();
            let response: serde_json::Value = serde_json::from_slice(&out).unwrap();
            assert_eq!(response["status"], status);
            let meters: Vec<_> = std::fs::read_dir(&quota.meters)
                .unwrap()
                .filter_map(Result::ok)
                .filter_map(|e| Meter::read(&e.path()))
                .collect();
            assert_eq!(meters.len(), 1);
            assert_eq!(
                (meters[0].app.as_str(), meters[0].owner.as_str()),
                ("actual-app", "sam")
            );
            assert_eq!(
                meters[0].retry_at.is_some(),
                limited,
                "{path}: {:?}",
                meters[0]
            );
            assert_eq!(meters[0].remaining, Some(123));
        }
        let quota = Config {
            app: "actual-app".into(),
            meters: tmp.path().join("forged"),
            kick: None,
        };
        let forged = json!({"owner":"sam","method":"GET","path":"/rate_limit","headers":[],"body":"","app":"other-app","retry_at":9999999999u64});
        assert!(
            answer(
                &config,
                &quota,
                forged.to_string().as_bytes(),
                Vec::new(),
                1793224800
            )
            .is_err()
        );
        assert!(!quota.meters.exists());
    }
}

#[cfg(test)]
mod extensions {
    use super::*;
    use crate::fake;
    use std::{net::TcpListener, thread, time::Instant};
    #[test]
    fn a_delayed_probe_extension_emits_another_host_wake() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let server = fake::serve(move |r| {
            if r.path.ends_with("/access_tokens") {
                return (
                    201,
                    vec![],
                    r#"{"token":"fixture","expires_at":"2099-01-01T00:00:00Z"}"#.into(),
                );
            }
            if r.path.starts_with("/app/installations") {
                return (200, vec![], fake::installations(&[(42, "sam")]));
            }
            if r.path == "/rate_limit" {
                return (
                    200,
                    vec![],
                    format!(
                        r#"{{"resources":{{"core":{{"remaining":10,"reset":{0}}},"graphql":{{"remaining":0,"reset":{0}}}}}}}"#,
                        now + 3600
                    ),
                );
            }
            (
                200,
                vec![],
                r#"{"errors":[{"type":"RATE_LIMITED"}]}"#.into(),
            )
        });
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("key"), fake::key(true)).unwrap();
        std::fs::create_dir(tmp.path().join("cache")).unwrap();
        let config = answer::Config {
            api: server.url.clone(),
            proxy: None,
            app_id: 7,
            key: tmp.path().join("key"),
            cache: tmp.path().join("cache"),
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let meters = tmp.path().join("meters");
        let observed = meters.clone();
        let quota = Config {
            app: "host".into(),
            meters,
            kick: Some(format!(
                "http://{}/kick/proj-watch",
                listener.local_addr().unwrap()
            )),
        };
        let watcher = thread::spawn(move || {
            let mut deadlines = vec![];
            let until = Instant::now() + Duration::from_secs(5);
            while deadlines.len() < 2 && Instant::now() < until {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::park_timeout(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut bytes = [0u8; 4096];
                let n = stream.read(&mut bytes).unwrap();
                assert!(String::from_utf8_lossy(&bytes[..n]).starts_with("POST /kick/proj-watch"));
                let meter = std::fs::read_dir(&observed)
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter_map(|e| Meter::read(&e.path()))
                    .next()
                    .unwrap();
                deadlines.push(meter.deadline().unwrap());
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                    .unwrap();
            }
            deadlines
        });
        let request = serde_json::json!({"owner":"sam","method":"POST","path":"/graphql","headers":[],"body":""});
        answer(
            &config,
            &quota,
            request.to_string().as_bytes(),
            Vec::new(),
            now as i64,
        )
        .unwrap();
        assert_eq!(watcher.join().unwrap(), vec![now + 60, now + 3600]);
    }
}
