//! `gh-token answer`: one request, read from stdin and answered on stdout.
//! systemd runs one for each connection to the socket (Accept=yes), as the
//! tool's own user, with the App's key a credential only that user can read:
//! whoever the socket lets in gets tokens, never the key.
//!
//! The request is a line, `OWNER` or `OWNER/REPO`. The answer is a line:
//! `token TOKEN`, `unavailable WHY` (ask again later) or `refused WHY`.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;

use crate::app::App;
use crate::cache;
use crate::error::Error;
use crate::target::Target;

pub struct Config {
    /// The REST API's root, and the proxy to it if there is one.
    pub api: String,
    pub proxy: Option<ureq::Proxy>,
    pub app_id: u64,
    /// The App's private key.
    pub key: PathBuf,
    /// Where minted tokens are kept (cache.rs).
    pub cache: PathBuf,
}

/// The longest request read: a target is shorter.
const MAX_REQUEST: u64 = 256;

pub fn answer(
    config: &Config,
    input: impl Read,
    mut output: impl Write,
    now: i64,
) -> io::Result<()> {
    let mut line = String::new();
    let reply = match BufReader::new(input.take(MAX_REQUEST)).read_line(&mut line) {
        Ok(_) => token(config, line.strip_suffix('\n').unwrap_or(&line), now),
        Err(e) => Err(Error::refused(format!("the request: {e}"))),
    };
    let reply = match reply {
        Ok(t) => format!("token {t}\n"),
        Err(e) => {
            eprintln!("gh-token: {:?}: {e}", line.trim_end());
            let why = e.what.replace('\n', " ");
            format!(
                "{} {why}\n",
                if e.unavailable {
                    "unavailable"
                } else {
                    "refused"
                }
            )
        }
    };
    output.write_all(reply.as_bytes())
}

fn token(config: &Config, request: &str, now: i64) -> Result<String, Error> {
    let target =
        Target::parse(request).ok_or_else(|| Error::refused("a request is OWNER or OWNER/REPO"))?;
    let key = target.key();
    if let Some(t) = cache::get(&config.cache, &key, now) {
        return Ok(t);
    }
    let pem = fs::read(&config.key)
        .map_err(|e| Error::refused(format!("the App's key, {}: {e}", config.key.display())))?;
    let minted =
        App::new(&config.api, config.proxy.clone(), config.app_id, &pem)?.mint(&target, now)?;
    // Only a cache: one that can't be written costs a mint next time.
    if let Err(e) = cache::put(&config.cache, &key, &minted) {
        eprintln!(
            "gh-token: couldn't keep {target}'s token in {}: {e}",
            config.cache.display()
        );
    }
    eprintln!("gh-token: minted a token for {target}");
    Ok(minted.token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake;

    fn ask(config: &Config, request: &str, now: i64) -> String {
        let mut out = Vec::new();
        answer(config, request.as_bytes(), &mut out, now).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn answers_from_github_then_from_what_it_kept() {
        let github = fake::serve(|r| match r.method.as_str() {
            "GET" => (200, vec![], fake::installations(&[(42, "sam")])),
            _ => (
                201,
                vec![],
                r#"{"token":"ghs_abc","expires_at":"2026-10-28T23:00:00Z"}"#.into(),
            ),
        });
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("key"), fake::key(true)).unwrap();
        fs::create_dir(dir.path().join("cache")).unwrap();
        let config = Config {
            api: github.url.clone(),
            proxy: None,
            app_id: 7,
            key: dir.path().join("key"),
            cache: dir.path().join("cache"),
        };
        let now = 1_793_224_800; // 22:00, an hour before it expires
        assert_eq!(ask(&config, "sam\n", now), "token ghs_abc\n");
        assert_eq!(github.asked().len(), 2);
        // Kept: the same token, without asking GitHub, until its margin.
        assert_eq!(ask(&config, "Sam", now + 60), "token ghs_abc\n");
        assert_eq!(github.asked().len(), 2);
        assert_eq!(ask(&config, "sam\n", now + 3000), "token ghs_abc\n");
        assert_eq!(github.asked().len(), 4);
        // A repo's is its own.
        ask(&config, "sam/r\n", now);
        assert_eq!(github.asked().len(), 6);
        // Not a target, or too long to be one: refused, GitHub never asked.
        for bad in ["sam/r/x\n", "\n", "", &format!("{}\n", "a".repeat(300))] {
            assert_eq!(
                ask(&config, bad, now),
                "refused a request is OWNER or OWNER/REPO\n",
                "{bad:?}"
            );
        }
        assert_eq!(github.asked().len(), 6);
        // No key: refused, and said on one line.
        let config = Config {
            key: dir.path().join("none"),
            cache: dir.path().join("empty"),
            ..config
        };
        let reply = ask(&config, "sam\n", now);
        assert!(
            reply.starts_with("refused the App's key")
                && reply.ends_with('\n')
                && reply.lines().count() == 1,
            "{reply}"
        );
    }
}
