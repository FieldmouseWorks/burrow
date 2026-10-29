//! The App's side of it: a JWT signed with its key, its installation on an
//! account, and an installation token minted there.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use rustls_pki_types::PrivateKeyDer;
use rustls_pki_types::pem::PemObject;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use ureq::tls::{RootCerts, TlsConfig};

use crate::error::Error;
use crate::target::Target;

pub struct App {
    agent: ureq::Agent,
    api: String,
    id: u64,
    key: RsaKeyPair,
}

/// A token, and when GitHub says it stops working (Unix seconds).
pub struct Minted {
    pub token: String,
    pub expires: i64,
}

/// Pages of installations read before giving up, a hundred to a page.
const MAX_PAGES: u32 = 50;

/// The most of an answer read.
const MAX_ANSWER: u64 = 1 << 20;

type Response = ureq::http::Response<ureq::Body>;

impl App {
    /// `api` is the REST API's root, `https://api.github.com` on github.com,
    /// reached through `proxy` or straight. `pem` is the App's private key,
    /// as GitHub gave it.
    pub fn new(api: &str, proxy: Option<ureq::Proxy>, id: u64, pem: &[u8]) -> Result<Self, Error> {
        // The parser's own errors can quote what it read: not repeated.
        let der = PrivateKeyDer::from_pem_slice(pem)
            .map_err(|_| Error::refused("the App's key isn't a PEM private key"))?;
        let key = match der {
            PrivateKeyDer::Pkcs1(k) => RsaKeyPair::from_der(k.secret_pkcs1_der()),
            PrivateKeyDer::Pkcs8(k) => RsaKeyPair::from_pkcs8(k.secret_pkcs8_der()),
            _ => return Err(Error::refused("the App's key isn't an RSA key")),
        }
        .map_err(|e| Error::refused(format!("the App's key: {e}")))?;
        let config = ureq::Agent::config_builder()
            .proxy(proxy)
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
            .user_agent("gh-token")
            .timeout_global(Some(Duration::from_secs(30)))
            // Every status is read here: which errors are GitHub's limits is
            // told from the answer.
            .http_status_as_error(false)
            .build();
        Ok(Self {
            agent: ureq::Agent::new_with_config(config),
            api: api.trim_end_matches('/').to_owned(),
            id,
            key,
        })
    }

    /// A token for the target: the App's installation on its owner, every
    /// repo of it or only its repo. `now` is Unix seconds.
    pub fn mint(&self, target: &Target, now: i64) -> Result<Minted, Error> {
        #[derive(Deserialize)]
        struct Token {
            token: String,
            expires_at: String,
        }
        let jwt = self.jwt(now)?;
        let id = self.installation(&jwt, &target.owner)?;
        let body = match &target.repo {
            Some(r) => serde_json::json!({ "repositories": [r] }),
            None => serde_json::json!({}),
        };
        let path = format!("/app/installations/{id}/access_tokens");
        let got: Token = self.post(&jwt, &path, &body)?;
        // Opaque, and as long as GitHub likes (its stateless ones are a JWT,
        // dots and all): only what can't break the line it goes back as, or
        // git's credential answer, is refused.
        if got.token.is_empty() || !got.token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::refused(format!(
                "{path}: GitHub's answer isn't a token"
            )));
        }
        let expires = OffsetDateTime::parse(&got.expires_at, &Rfc3339)
            .map_err(|e| Error::refused(format!("{path}: its expires_at: {e}")))?
            .unix_timestamp();
        Ok(Minted {
            token: got.token,
            expires,
        })
    }

    /// The App's own JWT, which only asks GitHub about the App.
    fn jwt(&self, now: i64) -> Result<String, Error> {
        let head = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        // A minute back for a clock ahead of GitHub's; nine on, as GitHub
        // takes ten at most.
        let claims =
            serde_json::json!({ "iat": now - 60, "exp": now + 540, "iss": self.id.to_string() });
        let signed = format!("{head}.{}", URL_SAFE_NO_PAD.encode(claims.to_string()));
        let mut sig = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &SystemRandom::new(),
                signed.as_bytes(),
                &mut sig,
            )
            .map_err(|_| Error::refused("couldn't sign with the App's key"))?;
        Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }

    /// The installation on `owner`, from every page of the App's: one
    /// GitHub didn't give is an error, never an installation missing.
    fn installation(&self, jwt: &str, owner: &str) -> Result<u64, Error> {
        #[derive(Deserialize)]
        struct Installation {
            id: u64,
            account: Option<Account>,
        }
        #[derive(Deserialize)]
        struct Account {
            login: String,
        }
        for page in 1..=MAX_PAGES {
            let got: Vec<Installation> =
                self.get(jwt, &format!("/app/installations?per_page=100&page={page}"))?;
            let on = |i: &&Installation| {
                i.account
                    .as_ref()
                    .is_some_and(|a| a.login.eq_ignore_ascii_case(owner))
            };
            if let Some(i) = got.iter().find(on) {
                return Ok(i.id);
            }
            if got.len() < 100 {
                return Err(Error::refused(format!(
                    "the App isn't installed on {owner}"
                )));
            }
        }
        Err(Error::refused(format!(
            "the App has more than {MAX_PAGES} pages of installations"
        )))
    }

    fn get<T: DeserializeOwned>(&self, jwt: &str, path: &str) -> Result<T, Error> {
        let url = format!("{}{path}", self.api);
        let got = self
            .agent
            .get(&url)
            .header("Authorization", &format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call();
        answer(&url, got)
    }

    fn post<T: DeserializeOwned>(
        &self,
        jwt: &str,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<T, Error> {
        let url = format!("{}{path}", self.api);
        let got = self
            .agent
            .post(&url)
            .header("Authorization", &format!("Bearer {jwt}"))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send_json(body);
        answer(&url, got)
    }
}

/// A success's JSON, or why there's none. GitHub's limits (a 429, a 403 its
/// headers or message say is one) and its own 5xx are unavailable, and so is
/// the network; anything else GitHub said no to is refused.
fn answer<T: DeserializeOwned>(url: &str, got: Result<Response, ureq::Error>) -> Result<T, Error> {
    let mut r = got.map_err(|e| unreachable(url, e))?;
    let status = r.status().as_u16();
    let limited =
        header(&r, "x-ratelimit-remaining") == Some("0") || header(&r, "retry-after").is_some();
    let body = r
        .body_mut()
        .with_config()
        .limit(MAX_ANSWER)
        .read_to_vec()
        .map_err(|e| unreachable(url, e))?;
    if (200..300).contains(&status) {
        return serde_json::from_slice(&body).map_err(|e| Error::refused(format!("{url}: {e}")));
    }
    #[derive(Deserialize)]
    struct Message {
        message: String,
    }
    let message = serde_json::from_slice::<Message>(&body)
        .map(|m| m.message)
        .unwrap_or_default();
    let what = format!("{url}: http status {status}: {message}");
    if status == 429
        || status >= 500
        || status == 403 && (limited || message.to_lowercase().contains("rate limit"))
    {
        Err(Error::unavailable(what))
    } else {
        Err(Error::refused(what))
    }
}

/// A request that got no answer. Only what asking again can fix is
/// unavailable: the network, DNS, a timeout, a connection dropped or stalled.
fn unreachable(url: &str, e: ureq::Error) -> Error {
    let what = format!("{url}: {e}");
    match e {
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed
        | ureq::Error::BodyStalled
        | ureq::Error::ConnectProxyFailed(_) => Error::unavailable(what),
        _ => Error::refused(what),
    }
}

fn header<'a>(r: &'a Response, name: &str) -> Option<&'a str> {
    r.headers().get(name).and_then(|v| v.to_str().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::{self, Fake};
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    const EXPIRES: &str = "2026-10-28T23:00:00Z";
    const EXPIRES_AT: i64 = 1_793_228_400;

    /// sam's installation (42) on page two of the App's, eve not installed.
    fn github(token: &'static str) -> Fake {
        fake::serve(move |r| match (r.method.as_str(), r.path.as_str()) {
            ("GET", "/app/installations?per_page=100&page=1") => {
                let others: Vec<(u64, String)> =
                    (0..100).map(|i| (i, format!("other{i}"))).collect();
                let others: Vec<(u64, &str)> =
                    others.iter().map(|(i, l)| (*i, l.as_str())).collect();
                (200, vec![], fake::installations(&others))
            }
            ("GET", "/app/installations?per_page=100&page=2") => (
                200,
                vec![],
                fake::installations(&[(42, "Sam"), (43, "zed")]),
            ),
            ("POST", "/app/installations/42/access_tokens") => (
                201,
                vec![],
                serde_json::json!({ "token": token, "expires_at": EXPIRES }).to_string(),
            ),
            _ => (404, vec![], r#"{"message":"Not Found"}"#.into()),
        })
    }

    fn app(github: &Fake, pem: &[u8]) -> App {
        App::new(&github.url, None, 7, pem).unwrap()
    }

    fn target(s: &str) -> Target {
        Target::parse(s).unwrap()
    }

    #[test]
    fn mints_a_token_for_an_account_or_one_repo() {
        let pem = fake::key(true);
        let github = github("ghs_abc");
        let got = app(&github, &pem).mint(&target("sam"), 1_000_000).unwrap();
        assert_eq!((got.token.as_str(), got.expires), ("ghs_abc", EXPIRES_AT));
        assert_eq!(
            github.asked(),
            [
                "GET /app/installations?per_page=100&page=1",
                "GET /app/installations?per_page=100&page=2",
                "POST /app/installations/42/access_tokens",
            ]
        );
        app(&github, &pem)
            .mint(&target("sam/R"), 1_000_000)
            .unwrap();
        let asked = github.asked.lock().unwrap();
        let body = |i: usize| serde_json::from_str::<serde_json::Value>(&asked[i].body).unwrap();
        assert_eq!(body(2), serde_json::json!({}));
        assert_eq!(body(5), serde_json::json!({ "repositories": ["R"] }));

        // Every call is the App's own, with a JWT GitHub can check.
        let key = match PrivateKeyDer::from_pem_slice(&pem).unwrap() {
            PrivateKeyDer::Pkcs1(k) => RsaKeyPair::from_der(k.secret_pkcs1_der()).unwrap(),
            _ => unreachable!(),
        };
        for r in asked.iter() {
            let jwt = r.auth.strip_prefix("Bearer ").unwrap();
            let (signed, sig) = jwt.rsplit_once('.').unwrap();
            let (head, claims) = signed.split_once('.').unwrap();
            let decode = |s: &str| String::from_utf8(URL_SAFE_NO_PAD.decode(s).unwrap()).unwrap();
            assert_eq!(decode(head), r#"{"alg":"RS256","typ":"JWT"}"#);
            let claims: serde_json::Value = serde_json::from_str(&decode(claims)).unwrap();
            assert_eq!(
                claims,
                serde_json::json!({ "iat": 999_940, "exp": 1_000_540, "iss": "7" })
            );
            UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.public().as_ref())
                .verify(signed.as_bytes(), &URL_SAFE_NO_PAD.decode(sig).unwrap())
                .unwrap();
        }
    }

    #[test]
    fn takes_a_pkcs8_key_too() {
        let github = github("ghs_abc");
        assert_eq!(
            app(&github, &fake::key(false))
                .mint(&target("sam"), 0)
                .unwrap()
                .token,
            "ghs_abc"
        );
    }

    #[test]
    fn refuses_a_key_that_isnt_one_without_quoting_it() {
        let github = github("ghs_abc");
        for pem in [
            &b""[..],
            b"-----BEGIN RSA PRIVATE KEY-----\nc2VjcmV0\n-----END RSA PRIVATE KEY-----\n",
        ] {
            let e = App::new(&github.url, None, 7, pem).err().unwrap();
            assert!(!e.unavailable && !e.what.contains("c2VjcmV0"), "{e}");
        }
    }

    #[test]
    fn an_account_without_the_app_is_refused() {
        let e = app(&github("ghs_abc"), &fake::key(true))
            .mint(&target("eve"), 0)
            .err()
            .unwrap();
        assert_eq!(e, Error::refused("the App isn't installed on eve"));
    }

    #[test]
    fn tells_github_unavailable_from_github_saying_no() {
        let pem = fake::key(true);
        for ((status, headers, message), unavailable) in [
            ((502, vec![], "Bad Gateway"), true),
            ((429, vec![], "slow down"), true),
            (
                (403, vec![("x-ratelimit-remaining", "0".to_owned())], "nope"),
                true,
            ),
            ((403, vec![("retry-after", "60".to_owned())], "nope"), true),
            (
                (403, vec![], "You have exceeded a secondary rate limit"),
                true,
            ),
            (
                (403, vec![], "Resource not accessible by integration"),
                false,
            ),
            (
                (401, vec![], "A JSON web token could not be decoded"),
                false,
            ),
            (
                (
                    422,
                    vec![],
                    "There is at least one repository that does not exist",
                ),
                false,
            ),
        ] {
            let github = fake::serve(move |r| {
                if r.method == "GET" {
                    (200, vec![], fake::installations(&[(42, "sam")]))
                } else {
                    (
                        status,
                        headers.clone(),
                        serde_json::json!({ "message": message }).to_string(),
                    )
                }
            });
            let e = app(&github, &pem).mint(&target("sam/r"), 0).err().unwrap();
            assert_eq!(e.unavailable, unavailable, "{status} {message}: {e}");
            assert!(e.what.contains(message), "{e}");
        }
        // Nobody there at all.
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", closed.local_addr().unwrap());
        drop(closed);
        let e = App::new(&url, None, 7, &pem)
            .unwrap()
            .mint(&target("sam"), 0)
            .err()
            .unwrap();
        assert!(e.unavailable, "{e}");
    }

    #[test]
    fn a_page_github_didnt_give_is_no_installation_missing() {
        let github = fake::serve(|r| match r.path.as_str() {
            "/app/installations?per_page=100&page=1" => {
                let all: Vec<(u64, String)> = (0..100).map(|i| (i, format!("x{i}"))).collect();
                let all: Vec<(u64, &str)> = all.iter().map(|(i, l)| (*i, l.as_str())).collect();
                (200, vec![], fake::installations(&all))
            }
            _ => (502, vec![], "{}".into()),
        });
        assert!(
            app(&github, &fake::key(true))
                .mint(&target("sam"), 0)
                .err()
                .unwrap()
                .unavailable
        );
    }

    #[test]
    fn takes_github_s_tokens_as_they_come() {
        let got = app(&github(fake::TOKEN), &fake::key(true))
            .mint(&target("sam"), 0)
            .unwrap();
        assert_eq!(got.token, fake::TOKEN);
    }

    #[test]
    fn hands_out_only_a_token() {
        for token in [
            "",
            "ghs_abc\nrefused x",
            "ghs abc",
            "ghs_abc\t",
            "ghs_\u{e9}",
        ] {
            let e = app(&github(token), &fake::key(true))
                .mint(&target("sam"), 0)
                .err()
                .unwrap();
            assert!(
                !e.unavailable && e.what.contains("isn't a token"),
                "{token:?}: {e}"
            );
        }
    }
}
