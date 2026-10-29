//! git's curl helper (git-remote-http, and git-remote-https, -ftp and -ftps,
//! its other names), as the App's env has git run it (`GIT_EXEC_PATH`): a
//! remote, whatever its host or scheme, hears only what the credential
//! helper gives git, which is the App's token for github.com and nothing for
//! any other host. What git would log in with instead, without asking the
//! helper:
//!
//! - the user and password in the URL, which goes without them;
//! - the headers `http.extraHeader` adds (an Authorization of the user's,
//!   say), a cookie jar's cookies (`http.cookieFile`), a TLS client
//!   certificate (`http.sslCert`), and the empty login that has curl answer
//!   a server's Negotiate with the user's Kerberos ticket
//!   (`http.emptyAuth`), from any config file or `-c`. Each is cleared
//!   last, for every URL and then for exactly that one: git takes a setting
//!   from the URLs that match it most closely, and of those the last one's
//!   word, and a URL git can't read matches only the first. A key
//!   (`http.sslKey`) is only ever read for a certificate, so it needs none;
//! - the certificate `GIT_SSL_CERT` names, which comes after git's config,
//!   so it goes;
//! - a login of the user's in their netrc, which curl answers a 401 with
//!   before git sees it: curl's netrc (NETRC, else ~/.netrc) is an empty
//!   one.
//!
//! None of it asks whether the host is GitHub's. git and curl each read a
//! host their own way (`github.com.`, `%67ithub.com`), so a reading of ours
//! that differed from theirs would let a login through.
//!
//! git-lfs does its own HTTP, sending git's headers and the netrc's login
//! by its own rules, and nothing like this can stop it. So in the App's env
//! it isn't run at all: `lfs` refuses it.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::error::Error;

/// Runs REAL, git's own helper, with `args`: REMOTE URL, as git runs it for
/// a remote, or URL alone, as it runs it to download a bundle. Returns only
/// when it couldn't.
pub fn run(real: &str, args: &[String], socket: Option<&Path>) -> Error {
    let given = std::env::var("GIT_CONFIG_PARAMETERS").ok();
    match command(real, args, socket, given.as_deref()) {
        Ok(mut helper) => Error::refused(format!("{real}: {}", helper.exec())),
        Err(e) => e,
    }
}

/// REAL with `args`, its URL's logins dropped as above: `given` is the
/// GIT_CONFIG_PARAMETERS it would have had. Without the socket, as it is.
fn command(
    real: &str,
    args: &[String],
    socket: Option<&Path>,
    given: Option<&str>,
) -> Result<Command, Error> {
    let mut helper = Command::new(real);
    if socket.is_none() {
        helper.args(args);
        return Ok(helper);
    }
    let url = match args {
        [remote, url] => {
            let url = without_login(url);
            helper.arg(remote).arg(&url);
            url
        }
        // A bundle's download, which names its URL again on the helper's
        // stdin (`get URL FILE`), where a login can't be dropped: run only
        // for a URL without one. A word that isn't a URL would be a
        // remote's name, whose URL git's config gives.
        [url] if url.contains("://") && without_login(url) == *url => {
            helper.arg(url);
            url.clone()
        }
        [_] => {
            return Err(Error::refused(
                "acting as the App, git's http helper isn't run for a URL with a login, or a remote's name, alone",
            ));
        }
        _ => {
            return Err(Error::refused(
                "git's http helper is given its URL, after its remote's name or alone",
            ));
        }
    };
    helper
        .env("GIT_CONFIG_PARAMETERS", cleared(given, &url))
        .env_remove("GIT_SSL_CERT")
        .env("NETRC", "/dev/null");
    Ok(helper)
}

/// git-lfs, REAL, with `args`, but never in the App's env (with the
/// socket). Returns only when it couldn't, or wouldn't.
pub fn lfs(real: &str, args: &[String], socket: Option<&Path>) -> Error {
    if socket.is_some() {
        return Error::refused(
            "acting as the App, git-lfs isn't run: it would send your own login (git's headers, your netrc) without asking for the App's",
        );
    }
    let e = Command::new(real).args(args).exec();
    Error::refused(format!("{real}: {e}"))
}

/// The URL without its user and password: its host ends at the first `/`,
/// `?` or `#`, and starts after the last `@` before that, so whichever `@`
/// git or curl takes for the end of a login, none is left.
fn without_login(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_owned();
    };
    let (authority, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("{scheme}://{host}{path}")
}

/// A path git reads as none: an optional one that's never there. Nothing
/// can make a file in /proc/self.
const NONE: &str = ":(optional)/proc/self/gh-token-none";

/// git's `http.` settings that log in without the credential helper, each
/// with the value that clears it: no headers, no cookie jar, no client
/// certificate, and no empty login for Negotiate.
const CLEARED: [(&str, &str); 4] = [
    ("extraheader", ""),
    ("cookiefile", NONE),
    ("sslcert", NONE),
    ("emptyauth", "false"),
];

/// GIT_CONFIG_PARAMETERS with each of CLEARED for every URL and then for
/// exactly `url`, after the rest, quoted as git quotes them
/// (`'key'='value'`).
fn cleared(given: Option<&str>, url: &str) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', r"'\''"));
    let mut all: Vec<String> = given
        .filter(|g| !g.trim().is_empty())
        .map(str::to_owned)
        .into_iter()
        .collect();
    for at in ["http".to_owned(), format!("http.{url}")] {
        for (key, value) in CLEARED {
            all.push(format!(
                "{}={}",
                quote(&format!("{at}.{key}")),
                quote(value)
            ));
        }
    }
    all.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn drops_a_url_s_login_whatever_its_host() {
        for (url, bare) in [
            (
                "https://github.com/sam/r.git",
                "https://github.com/sam/r.git",
            ),
            (
                "https://peter:tok@github.com/sam/r.git",
                "https://github.com/sam/r.git",
            ),
            (
                "https://peter@GitHub.com:0443/sam/r",
                "https://GitHub.com:0443/sam/r",
            ),
            (
                "HTTP://peter:tok@github.com./sam/r",
                "HTTP://github.com./sam/r",
            ),
            (
                "https://peter@%67ithub.com/sam/r",
                "https://%67ithub.com/sam/r",
            ),
            (
                "https://peter:tok@gitlab.com/sam/r",
                "https://gitlab.com/sam/r",
            ),
            // Every `@` before the host ends, whichever one ends a login.
            (
                "https://peter@github.com@eve.com/sam/r",
                "https://eve.com/sam/r",
            ),
            ("https://a@b:c@github.com", "https://github.com"),
            // Not a login: the `@` is past the host.
            ("https://github.com/sam/r@x", "https://github.com/sam/r@x"),
            (
                "https://github.com?x@eve.com/",
                "https://github.com?x@eve.com/",
            ),
            (
                "https://github.com#x@eve.com",
                "https://github.com#x@eve.com",
            ),
            ("sam/r", "sam/r"),
        ] {
            assert_eq!(without_login(url), bare, "{url}");
        }
    }

    /// CLEARED, for every URL and then for `url`, as `cleared` sets them.
    fn every_one(url: &str) -> String {
        let at = |at: &str| {
            format!(
                "'{at}.extraheader'='' '{at}.cookiefile'='{NONE}' '{at}.sslcert'='{NONE}' '{at}.emptyauth'='false'"
            )
        };
        format!("{} {}", at("http"), at(&format!("http.{url}")))
    }

    #[test]
    fn clears_each_login_git_needs_no_helper_for_last() {
        let u = "https://github.com/sam/r.git";
        assert_eq!(cleared(None, u), every_one(u));
        assert_eq!(cleared(Some(" "), u), cleared(None, u));
        assert_eq!(
            cleared(Some("'http.extraheader'='Authorization: x'"), u),
            format!("'http.extraheader'='Authorization: x' {}", every_one(u))
        );
        assert!(
            cleared(None, "https://github.com/it's")
                .contains(r"'http.https://github.com/it'\''s.extraheader'=''")
        );
    }

    #[test]
    fn runs_git_s_helper_with_no_login_of_the_user_s() {
        let socket = Some(Path::new("/run/gh-token/workers.sock"));
        let run =
            |a: &[&str], socket| command("git-remote-https", &args(a), socket, Some("'a'='b'"));
        let env = |c: &Command, name: &str| {
            c.get_envs()
                .find(|(k, _)| *k == OsStr::new(name))
                .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
        };
        let parameters = |c: &Command| env(c, "GIT_CONFIG_PARAMETERS");
        let removed = |c: &Command, name: &str| {
            c.get_envs()
                .any(|(k, v)| k == OsStr::new(name) && v.is_none())
        };
        // Any host, in any spelling, over https or http.
        for (url, bare) in [
            (
                "https://peter:tok@github.com/sam/r.git",
                "https://github.com/sam/r.git",
            ),
            (
                "https://peter@github.com./sam/r",
                "https://github.com./sam/r",
            ),
            ("https://%67ithub.com/sam/r", "https://%67ithub.com/sam/r"),
            ("http://peter@github.com/sam/r", "http://github.com/sam/r"),
            (
                "https://peter:tok@gitlab.com/sam/r",
                "https://gitlab.com/sam/r",
            ),
        ] {
            let c = run(&["origin", url], socket).unwrap();
            assert_eq!(c.get_args().collect::<Vec<_>>(), ["origin", bare], "{url}");
            assert_eq!(
                parameters(&c),
                Some(format!("'a'='b' {}", every_one(bare))),
                "{url}"
            );
            assert_eq!(env(&c, "NETRC").as_deref(), Some("/dev/null"), "{url}");
            assert!(removed(&c, "GIT_SSL_CERT"), "{url}");
        }
        // No socket: git's, as it is.
        let a = ["origin", "https://peter:tok@github.com/sam/r"];
        let c = run(&a, None).unwrap();
        assert_eq!(c.get_args().collect::<Vec<_>>(), a);
        assert_eq!(parameters(&c), None);
        assert_eq!(env(&c, "NETRC"), None);
        assert!(!removed(&c, "GIT_SSL_CERT"));
        // A bundle's download: the URL alone, cleared the same way, but
        // never one with a login (git sends it again on stdin), nor a
        // remote's name in its place.
        let bundle = "https://github.com/sam/r.bundle";
        let c = run(&[bundle], socket).unwrap();
        assert_eq!(c.get_args().collect::<Vec<_>>(), [bundle]);
        assert_eq!(
            parameters(&c),
            Some(format!("'a'='b' {}", every_one(bundle)))
        );
        assert_eq!(env(&c, "NETRC").as_deref(), Some("/dev/null"));
        assert!(removed(&c, "GIT_SSL_CERT"));
        for refused in [
            "https://peter:tok@github.com/sam/r.bundle",
            "https://peter@github.com@eve.com/x",
            "origin",
            "",
        ] {
            assert!(run(&[refused], socket).is_err(), "{refused}");
        }
        // Without the socket, as git gives it.
        let c = run(&["https://peter:tok@github.com/sam/r.bundle"], None).unwrap();
        assert_eq!(
            c.get_args().collect::<Vec<_>>(),
            ["https://peter:tok@github.com/sam/r.bundle"]
        );
        // Nothing, or more than git gives: refused.
        assert!(run(&[], socket).is_err());
        assert!(run(&["origin", bundle, "x"], socket).is_err());
    }

    #[test]
    fn never_runs_git_lfs_as_the_app() {
        let e = lfs(
            "/nonexistent/git-lfs",
            &args(&["pre-push", "origin"]),
            Some(Path::new("/run/gh-token/workers.sock")),
        );
        assert!(e.what.contains("acting as the App"), "{e}");
        // Outside the env it's git-lfs, as it is: here, one that isn't there.
        let e = lfs("/nonexistent/git-lfs", &args(&["version"]), None);
        assert!(e.what.starts_with("/nonexistent/git-lfs: "), "{e}");
    }
}
