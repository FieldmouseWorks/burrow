//! The routes, as a browser meets them: the ceremonies over JSON with a
//! software authenticator, the cookie, the redirects and the headers.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::body::{Body, to_bytes};
use axum::http::Request;
use serde_json::Value;
use tower::ServiceExt;
use webauthn_authenticator_rs::WebauthnAuthenticator;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse, Url};

use super::*;
use crate::Owner;
use crate::enroll::Codes;

const ORIGIN: &str = "https://box.tail1.ts.net";
const IDLE: u64 = 3600;
const MAX: u64 = 86400;

struct Rig {
    app: Arc<App>,
    clock: Arc<AtomicU64>,
    _state: tempfile::TempDir,
    _fonts: tempfile::TempDir,
}

fn rig() -> Rig {
    let state = tempfile::tempdir().unwrap();
    let fonts = crate::assets::tests::fonts();
    let cfg = Config {
        title: "box".into(),
        origin: ORIGIN.into(),
        port: 0,
        state: state.path().into(),
        shell: crate::assets::tests::shell().into(),
        fonts: fonts.path().into(),
        owner: Owner {
            id: 583231,
            login: "sam".into(),
        },
        idle_timeout: IDLE,
        max_age: MAX,
    };
    let clock = Arc::new(AtomicU64::new(1_000_000));
    let c = clock.clone();
    let app = Arc::new(App::with_clock(cfg, Box::new(move || c.load(Ordering::SeqCst))).unwrap());
    Rig {
        app,
        clock,
        _state: state,
        _fonts: fonts,
    }
}

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    fn header(&self, name: &str) -> &str {
        self.headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
    }

    /// The session token the answer sets.
    fn token(&self) -> String {
        let set = self.header("set-cookie");
        let pair = set.split(';').next().unwrap();
        pair.strip_prefix(&format!("{COOKIE}=")).unwrap().to_owned()
    }
}

impl Rig {
    async fn send(&self, req: Request<Body>) -> Answer {
        let res = router(self.app.clone()).oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = to_bytes(res.into_body(), usize::MAX).await.unwrap();
        Answer {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    async fn get(&self, path: &str, token: Option<&str>) -> Answer {
        let mut req = Request::get(path);
        if let Some(t) = token {
            req = req.header("cookie", format!("other=1; {COOKIE}={t}"));
        }
        self.send(req.body(Body::empty()).unwrap()).await
    }

    async fn post(&self, path: &str, origin: Option<&str>, body: Value) -> Answer {
        let mut req = Request::post(path).header("content-type", "application/json");
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        self.send(req.body(Body::from(body.to_string())).unwrap())
            .await
    }

    fn tick(&self, secs: u64) {
        self.clock.fetch_add(secs, Ordering::SeqCst);
    }

    fn code(&self) -> String {
        let now = self.clock.load(Ordering::SeqCst);
        Codes::new(&self.app.cfg.state, self.app.cfg.owner.id)
            .issue(now)
            .unwrap()
    }

    /// Enrol `dev` the way the page does: the session's token.
    async fn enrol(&self, dev: &mut SoftPasskey, name: &str) -> String {
        let code = self.code();
        let start = self
            .post(
                "/enroll/start",
                Some(ORIGIN),
                json!({ "code": code, "name": name }),
            )
            .await;
        assert_eq!(start.status, StatusCode::OK, "{}", start.body);
        let s = start.json();
        let options: CreationChallengeResponse =
            serde_json::from_value(s["options"].clone()).unwrap();
        let credential = dev
            .do_registration(Url::parse(ORIGIN).unwrap(), options)
            .unwrap();
        let finish = self
            .post(
                "/enroll/finish",
                Some(ORIGIN),
                json!({ "ceremony": s["ceremony"], "credential": credential }),
            )
            .await;
        assert_eq!(finish.status, StatusCode::OK, "{}", finish.body);
        assert_eq!(finish.json()["next"], "/");
        finish.token()
    }

    async fn login(&self, dev: &mut SoftPasskey) -> Answer {
        let start = self.post("/login/start", Some(ORIGIN), json!({})).await;
        assert_eq!(start.status, StatusCode::OK, "{}", start.body);
        let s = start.json();
        let options: RequestChallengeResponse =
            serde_json::from_value(s["options"].clone()).unwrap();
        let credential = dev
            .do_authentication(Url::parse(ORIGIN).unwrap(), options)
            .unwrap();
        self.post(
            "/login/finish",
            Some(ORIGIN),
            json!({ "ceremony": s["ceremony"], "credential": credential }),
        )
        .await
    }
}

#[tokio::test]
async fn a_page_without_a_session_sends_you_to_sign_in() {
    let rig = rig();
    for token in [None, Some("forged")] {
        let a = rig.get("/", token).await;
        assert_eq!(a.status, StatusCode::SEE_OTHER);
        assert_eq!(a.header("location"), "/login");
    }
    let a = rig.get("/login", None).await;
    assert_eq!(a.status, StatusCode::OK);
    // With nothing enrolled, sign-in points at enrolment.
    assert!(a.body.contains("href=\"/enroll\""));
    assert!(!a.body.contains("data-login"));
}

#[tokio::test]
async fn enrolment_refuses_a_wrong_code() {
    let rig = rig();
    rig.code();
    let a = rig
        .post(
            "/enroll/start",
            Some(ORIGIN),
            json!({ "code": "ABCDE-FGHJK" }),
        )
        .await;
    assert_eq!(a.status, StatusCode::FORBIDDEN);
    assert!(a.json()["error"].as_str().unwrap().contains("4 more tries"));
    assert!(a.json().get("options").is_none());
}

#[tokio::test]
async fn a_post_from_anywhere_else_is_turned_away() {
    let rig = rig();
    let code = rig.code();
    for origin in [
        None,
        Some("https://elsewhere.example"),
        Some("http://box.tail1.ts.net"),
    ] {
        let a = rig
            .post("/enroll/start", origin, json!({ "code": code }))
            .await;
        assert_eq!(a.status, StatusCode::FORBIDDEN, "{origin:?}");
        assert_eq!(a.json()["error"], "Not from this dashboard.");
    }
    // The code is no worse for it.
    let a = rig
        .post("/enroll/start", Some(ORIGIN), json!({ "code": code }))
        .await;
    assert_eq!(a.status, StatusCode::OK);
}

#[tokio::test]
async fn enrol_sign_in_and_out() {
    let rig = rig();
    let mut phone = SoftPasskey::new(true);
    let first = rig.enrol(&mut phone, "Phone").await;

    let home = rig.get("/", Some(&first)).await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("sam"));
    assert!(home.body.contains("Phone"));
    // Signed in, the sign-in page sends you home.
    assert_eq!(
        rig.get("/login", Some(&first)).await.header("location"),
        "/"
    );

    let login = rig.login(&mut phone).await;
    assert_eq!(login.status, StatusCode::OK, "{}", login.body);
    let set = login.header("set-cookie").to_owned();
    for flag in [
        "Path=/",
        "Secure",
        "HttpOnly",
        "SameSite=Strict",
        &format!("Max-Age={MAX}"),
    ] {
        assert!(set.split("; ").any(|f| f == flag), "{flag} in {set}");
    }
    let token = login.token();

    let mut req = Request::post("/logout").header("origin", ORIGIN);
    req = req.header("cookie", format!("{COOKIE}={token}"));
    let out = rig.send(req.body(Body::empty()).unwrap()).await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    assert_eq!(out.header("location"), "/login");
    assert!(out.header("set-cookie").contains("Max-Age=0"));
    assert_eq!(
        rig.get("/", Some(&token)).await.status,
        StatusCode::SEE_OTHER
    );
    // The other session is its own.
    assert_eq!(rig.get("/", Some(&first)).await.status, StatusCode::OK);
}

#[tokio::test]
async fn a_session_ends_when_idle_and_at_the_cap() {
    let rig = rig();
    let mut dev = SoftPasskey::new(true);
    let token = rig.enrol(&mut dev, "").await;
    let mut used = 0;
    while used + IDLE / 2 < MAX {
        rig.tick(IDLE / 2);
        used += IDLE / 2;
        assert_eq!(
            rig.get("/", Some(&token)).await.status,
            StatusCode::OK,
            "at {used}"
        );
    }
    rig.tick(MAX - used);
    assert_eq!(
        rig.get("/", Some(&token)).await.status,
        StatusCode::SEE_OTHER
    );

    let token = rig.login(&mut dev).await.token();
    rig.tick(IDLE);
    assert_eq!(
        rig.get("/", Some(&token)).await.status,
        StatusCode::SEE_OTHER
    );
}

#[tokio::test]
async fn a_bad_answer_is_said_plainly() {
    let rig = rig();
    let a = rig.post("/login/start", Some(ORIGIN), json!({})).await;
    assert_eq!(a.status, StatusCode::CONFLICT);
    assert!(a.json()["error"].as_str().unwrap().contains("No passkey"));
    let mut dev = SoftPasskey::new(true);
    rig.enrol(&mut dev, "").await;
    let a = rig
        .post(
            "/login/finish",
            Some(ORIGIN),
            json!({ "ceremony": "nope", "credential": {} }),
        )
        .await;
    assert!(a.status.is_client_error());
}

#[tokio::test]
async fn every_answer_carries_the_security_headers() {
    let rig = rig();
    for path in ["/login", "/", "/nope", "/sw.js"] {
        let a = rig.get(path, None).await;
        assert!(
            a.header("content-security-policy")
                .contains("script-src 'self'"),
            "{path}"
        );
        assert_eq!(a.header("x-frame-options"), "DENY", "{path}");
        assert_eq!(a.header("x-content-type-options"), "nosniff", "{path}");
        // A form's POST keeps its Origin, as same_origin needs.
        assert_eq!(a.header("referrer-policy"), "same-origin", "{path}");
    }
    assert_eq!(
        rig.get("/login", None).await.header("cache-control"),
        "no-store"
    );
    assert_eq!(rig.get("/nope", None).await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_shell_installs() {
    let rig = rig();
    let v = rig.app.assets.version.clone();
    let css = rig.get(&format!("/assets/{v}/app.css"), None).await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(css.header("cache-control").contains("immutable"));
    assert!(css.header("content-type").starts_with("text/css"));
    let stale = rig.get("/assets/0ld/app.css", None).await;
    assert_eq!(stale.header("cache-control"), "no-cache");
    assert_eq!(
        rig.get(&format!("/assets/{v}/../../etc/passwd"), None)
            .await
            .status,
        StatusCode::NOT_FOUND
    );

    let sw = rig.get("/sw.js", None).await;
    assert_eq!(sw.header("cache-control"), "no-cache");
    assert!(sw.body.contains(&v));

    let m = rig.get("/manifest.webmanifest", None).await;
    assert_eq!(m.header("content-type"), "application/manifest+json");
    assert_eq!(m.json()["display"], "standalone");

    let login = rig.get("/login", None).await.body;
    for link in [
        "rel=\"manifest\"",
        "rel=\"apple-touch-icon\"",
        "apple-mobile-web-app-capable",
    ] {
        assert!(login.contains(link), "{link}");
    }
    let offline = rig.get("/offline", None).await;
    assert_eq!(offline.status, StatusCode::OK);
    assert!(offline.body.contains("data-retry"));
}
