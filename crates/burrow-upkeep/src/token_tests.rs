//! The App's tokens: minted with its key, as the App the config names, and
//! kept nowhere.

use std::fs;
use std::path::PathBuf;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use gh_token::fake::{self, Fake};

use super::*;
use crate::testing::config;

const NOW: i64 = 1_793_224_800;
/// An hour from NOW.
const EXPIRES: &str = "2026-10-28T23:00:00Z";
const EXPIRES_AT: i64 = 1_793_228_400;

/// The App on Sam (as GitHub spells the login) and not on eve; nothing
/// answering but a 502 with `down`.
fn github(down: bool) -> Fake {
    fake::serve(move |r| match (r.method.as_str(), r.path.as_str()) {
        _ if down => (502, vec![], r#"{"message":"Bad Gateway"}"#.into()),
        ("GET", "/app/installations?per_page=100&page=1") => {
            (200, vec![], fake::installations(&[(11, "Sam")]))
        }
        ("POST", "/app/installations/11/access_tokens") => (
            201,
            vec![],
            serde_json::json!({ "token": "ghs_sam", "expires_at": EXPIRES }).to_string(),
        ),
        _ => (404, vec![], r#"{"message":"Not Found"}"#.into()),
    })
}

/// The steward's config on `github`, and its key where systemd puts its
/// credentials.
fn steward(github: &Fake) -> (tempfile::TempDir, Config, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path());
    cfg.api.clone_from(&github.url);
    fs::create_dir_all(&cfg.state_dir).unwrap();
    let creds = dir.path().join("creds");
    fs::create_dir(&creds).unwrap();
    fs::write(creds.join("app-key"), fake::key(true)).unwrap();
    (dir, cfg, creds)
}

/// Every file under `dir`, read whole.
fn files(dir: &Path) -> Vec<String> {
    let mut all = vec![];
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            all.extend(files(&p));
        } else {
            all.push(String::from_utf8_lossy(&fs::read(&p).unwrap()).into_owned());
        }
    }
    all
}

#[test]
fn mints_an_owners_token_as_the_app_the_config_names_and_keeps_it_nowhere() {
    let github = github(false);
    let (dir, cfg, creds) = steward(&github);
    let got = token(&cfg, "sam", &creds, None, NOW);
    let Ok(Token::Minted(m)) = got else {
        panic!("no token for sam");
    };
    assert_eq!((m.token.as_str(), m.expires), ("ghs_sam", EXPIRES_AT));
    assert_eq!(
        github.asked(),
        [
            "GET /app/installations?per_page=100&page=1",
            "POST /app/installations/11/access_tokens",
        ]
    );
    // As App 7, the config's.
    let asked = github.asked.lock().unwrap();
    let claims = asked[0].auth.split('.').nth(1).unwrap();
    let claims: serde_json::Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(claims).unwrap()).unwrap();
    assert_eq!(claims["iss"], "7");
    drop(asked);
    // Written nowhere: the state dir, and the rest, are the machine user's.
    assert!(
        files(dir.path()).iter().all(|f| !f.contains("ghs_sam")),
        "a token kept on disk"
    );
    // Each ask mints: nothing kept to hand out again.
    assert!(matches!(
        token(&cfg, "Sam", &creds, None, NOW + 60),
        Ok(Token::Minted(_))
    ));
    assert_eq!(github.asked().len(), 4);
}

#[test]
fn an_owner_the_app_isnt_on_has_none_to_mint() {
    let github = github(false);
    let (_dir, cfg, creds) = steward(&github);
    let got = token(&cfg, "eve", &creds, None, NOW);
    assert!(matches!(got, Ok(Token::NotInstalled)));
    assert_eq!(
        github.asked(),
        ["GET /app/installations?per_page=100&page=1"]
    );
}

#[test]
fn github_not_answering_is_asking_again_later() {
    let github = github(true);
    let (_dir, cfg, creds) = steward(&github);
    match token(&cfg, "sam", &creds, None, NOW) {
        Err(e) => assert!(e.unavailable, "{e}"),
        Ok(_) => panic!("a token with GitHub down"),
    }
}

#[test]
fn refuses_what_isnt_an_owner_and_a_pass_without_the_key() {
    let github = github(false);
    let (_dir, cfg, creds) = steward(&github);
    for owner in ["sam/r", "", "-sam", "sam name"] {
        match token(&cfg, owner, &creds, None, NOW) {
            Err(e) => assert!(!e.unavailable && e.what.contains("isn't an owner"), "{e}"),
            Ok(_) => panic!("a token for {owner:?}"),
        }
    }
    assert!(github.asked().is_empty());
    // Not given the credential: refused, and nothing asked of GitHub.
    fs::remove_file(creds.join("app-key")).unwrap();
    match token(&cfg, "sam", &creds, None, NOW) {
        Err(e) => assert!(!e.unavailable && e.what.contains("the App's key"), "{e}"),
        Ok(_) => panic!("a token without the key"),
    }
    assert!(github.asked().is_empty());
}
