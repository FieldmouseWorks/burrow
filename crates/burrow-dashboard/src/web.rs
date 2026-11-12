//! The routes. Only localhost reaches them (the module binds there and
//! `tailscale serve` forwards), so every request has come through the
//! tailnet's HTTPS or from the machine itself. A page needs a live session;
//! one without is sent to sign in. Every POST has to come from the
//! dashboard's own origin, on top of the cookie's SameSite=Strict.

use std::sync::{Arc, Mutex, MutexGuard};

use axum::Json;
use axum::Router;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use serde::Deserialize;
use serde_json::json;
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};

use crate::assets::Assets;
use crate::core::{Core, Error, Live};
use crate::pages::{self, Frame};
use crate::{Config, now};

/// The session cookie. `__Host-` makes the browser hold it to this host,
/// path / and Secure.
pub const COOKIE: &str = "__Host-session";

const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
    font-src 'self'; connect-src 'self'; manifest-src 'self'; worker-src 'self'; \
    form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

pub struct App {
    pub cfg: Config,
    pub assets: Assets,
    core: Mutex<Core>,
    /// The clock: seconds since the epoch.
    clock: Box<dyn Fn() -> u64 + Send + Sync>,
}

impl App {
    pub fn new(cfg: Config) -> Result<App, String> {
        App::with_clock(cfg, Box::new(now))
    }

    pub fn with_clock(
        cfg: Config,
        clock: Box<dyn Fn() -> u64 + Send + Sync>,
    ) -> Result<App, String> {
        let assets = Assets::load(&cfg.shell, &cfg.fonts)?;
        let core = Mutex::new(Core::open(&cfg)?);
        Ok(App {
            cfg,
            assets,
            core,
            clock,
        })
    }

    fn core(&self) -> MutexGuard<'_, Core> {
        // A panic mid-request leaves nothing half-written that matters:
        // every file is replaced whole.
        self.core.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn now(&self) -> u64 {
        (self.clock)()
    }

    fn frame(&self) -> Frame<'_> {
        Frame {
            title: &self.cfg.title,
            host: self.cfg.rp_id().unwrap_or_default(),
            assets: &self.assets,
        }
    }

    /// The live session the request's cookie names.
    fn session(&self, headers: &HeaderMap) -> Result<Option<(String, Live)>, Error> {
        let Some(token) = cookie(headers, COOKIE) else {
            return Ok(None);
        };
        let now = self.now();
        Ok(self.core().session(&token, now)?.map(|live| (token, live)))
    }
}

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/", get(home))
        .route("/login", get(login_page))
        .route("/login/start", post(login_start))
        .route("/login/finish", post(login_finish))
        .route("/enroll", get(enrol_page))
        .route("/enroll/start", post(enrol_start))
        .route("/enroll/finish", post(enrol_finish))
        .route("/logout", post(logout))
        .route("/offline", get(offline))
        .route("/manifest.webmanifest", get(manifest))
        .route("/sw.js", get(service_worker))
        .route("/favicon.ico", get(favicon))
        .route("/apple-touch-icon.png", get(touch_icon))
        .route("/assets/{version}/{*file}", get(asset))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(app.clone(), same_origin))
        .layer(middleware::from_fn(secure))
        .with_state(app)
}

/// Turn away a POST from anywhere but the dashboard's own pages.
async fn same_origin(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    if req.method() == Method::POST
        && req
            .headers()
            .get(header::ORIGIN)
            .and_then(|o| o.to_str().ok())
            != Some(&app.cfg.origin)
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Not from this dashboard." })),
        )
            .into_response();
    }
    next.run(req).await
}

/// The headers every answer carries.
async fn secure(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    let mut set = |name: &'static str, value: &'static str| {
        h.entry(name).or_insert(HeaderValue::from_static(value));
    };
    set("content-security-policy", CSP);
    set("x-content-type-options", "nosniff");
    // Not no-referrer: under it a browser sends a form's POST (sign out)
    // with `Origin: null`, and `same_origin` turns it away. Same-origin
    // still keeps every address here from going anywhere else.
    set("referrer-policy", "same-origin");
    set("x-frame-options", "DENY");
    set("cross-origin-opener-policy", "same-origin");
    set("cross-origin-resource-policy", "same-origin");
    set(
        "permissions-policy",
        "camera=(), microphone=(), geolocation=(), payment=()",
    );
    set("strict-transport-security", "max-age=31536000");
    set("cache-control", "no-store");
    res
}

/// The value of the cookie `name`.
pub fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_owned())
}

fn session_cookie(token: &str, max_age: u64) -> String {
    format!("{COOKIE}={token}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age}")
}

fn page(markup: maud::Markup) -> Response {
    Html(markup.into_string()).into_response()
}

fn failed(e: Error) -> Response {
    let code = match e {
        Error::NoCode | Error::WrongCode { .. } => StatusCode::FORBIDDEN,
        Error::NoPasskeys | Error::Enrolled => StatusCode::CONFLICT,
        Error::NoCeremony => StatusCode::GONE,
        Error::Rejected(_) => StatusCode::UNAUTHORIZED,
        Error::Io(ref e) => {
            eprintln!("burrow-dashboard: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (code, Json(json!({ "error": e.to_string() }))).into_response()
}

/// A signed-in answer: the session's cookie, and where to go next.
fn signed_in(app: &App, token: Result<String, Error>) -> Response {
    match token {
        Ok(token) => (
            [(header::SET_COOKIE, session_cookie(&token, app.cfg.max_age))],
            Json(json!({ "next": "/" })),
        )
            .into_response(),
        Err(e) => failed(e),
    }
}

async fn home(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    match app.session(&headers) {
        Ok(Some((_, live))) => {
            let now = app.now();
            let core = app.core();
            page(pages::home(
                &app.frame(),
                &app.cfg.owner.login,
                &live,
                core.limits(),
                core.passkeys(),
                now,
            ))
        }
        Ok(None) => Redirect::to("/login").into_response(),
        Err(e) => failed(e),
    }
}

async fn login_page(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    match app.session(&headers) {
        Ok(Some(_)) => Redirect::to("/").into_response(),
        Ok(None) => {
            let any = !app.core().passkeys().is_empty();
            page(pages::login(&app.frame(), any))
        }
        Err(e) => failed(e),
    }
}

async fn login_start(State(app): State<Arc<App>>) -> Response {
    let now = app.now();
    match app.core().login_start(now) {
        Ok((ceremony, options)) => {
            Json(json!({ "ceremony": ceremony, "options": options })).into_response()
        }
        Err(e) => failed(e),
    }
}

#[derive(Deserialize)]
struct LoginAnswer {
    ceremony: String,
    credential: PublicKeyCredential,
}

async fn login_finish(State(app): State<Arc<App>>, Json(a): Json<LoginAnswer>) -> Response {
    let now = app.now();
    let token = app.core().login_finish(&a.ceremony, &a.credential, now);
    signed_in(&app, token)
}

async fn enrol_page(State(app): State<Arc<App>>) -> Response {
    page(pages::enrol(&app.frame()))
}

#[derive(Deserialize)]
struct EnrolStart {
    code: String,
    #[serde(default)]
    name: String,
}

async fn enrol_start(State(app): State<Arc<App>>, Json(s): Json<EnrolStart>) -> Response {
    let now = app.now();
    match app.core().enrol_start(&s.code, &s.name, now) {
        Ok((ceremony, options)) => {
            Json(json!({ "ceremony": ceremony, "options": options })).into_response()
        }
        Err(e) => failed(e),
    }
}

#[derive(Deserialize)]
struct EnrolAnswer {
    ceremony: String,
    credential: RegisterPublicKeyCredential,
}

async fn enrol_finish(State(app): State<Arc<App>>, Json(a): Json<EnrolAnswer>) -> Response {
    let now = app.now();
    let token = app.core().enrol_finish(&a.ceremony, &a.credential, now);
    signed_in(&app, token)
}

async fn logout(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    if let Some(token) = cookie(&headers, COOKIE)
        && let Err(e) = app.core().logout(&token)
    {
        return failed(e);
    }
    (
        [(header::SET_COOKIE, session_cookie("", 0))],
        Redirect::to("/login"),
    )
        .into_response()
}

async fn offline(State(app): State<Arc<App>>) -> Response {
    page(pages::offline(&app.frame()))
}

async fn not_found(State(app): State<Arc<App>>) -> Response {
    (StatusCode::NOT_FOUND, page(pages::not_found(&app.frame()))).into_response()
}

async fn manifest(State(app): State<Arc<App>>) -> Response {
    (
        [(header::CONTENT_TYPE, "application/manifest+json")],
        app.assets.manifest(&app.cfg.title),
    )
        .into_response()
}

async fn service_worker(State(app): State<Arc<App>>) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        app.assets.service_worker(),
    )
        .into_response()
}

fn file(app: &App, name: &str, cache: &'static str) -> Response {
    match app.assets.get(name) {
        Some((bytes, kind)) => (
            [(header::CONTENT_TYPE, kind), (header::CACHE_CONTROL, cache)],
            bytes.to_vec(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn favicon(State(app): State<Arc<App>>) -> Response {
    file(&app, "favicon-32.png", "public, max-age=86400")
}

async fn touch_icon(State(app): State<Arc<App>>) -> Response {
    file(&app, "apple-touch-icon.png", "public, max-age=86400")
}

/// An asset under this shell's version is the same for good; under any
/// other (a page from before a restart) it's this shell's, not to be kept.
async fn asset(
    State(app): State<Arc<App>>,
    Path((version, name)): Path<(String, String)>,
) -> Response {
    let cache = if version == app.assets.version {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    file(&app, &name, cache)
}

#[cfg(test)]
#[path = "web_tests.rs"]
mod tests;
