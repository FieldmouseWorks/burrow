//! git's http and https (git-remote-http, git-remote-https), as the App's
//! env has git run them (`GIT_EXEC_PATH`): a github.com remote hears only
//! what the credential helper gives git, the App's token. Three things git
//! would send instead, without asking the helper: the user and password in
//! the URL; the headers `http.extraHeader` adds (an Authorization of the
//! user's, say), from any config file or `-c`, for any URL that matches; and
//! a login of the user's in their netrc, which curl answers GitHub's 401
//! with before git sees it. So the URL goes without its user and password,
//! the headers are cleared by an empty one for exactly that URL, set last
//! (git takes the headers of the URLs that match it most closely, and of
//! those the last one's word), and curl's netrc (NETRC, else ~/.netrc) is
//! an empty one. Plain http to GitHub isn't run at all.
//!
//! git-lfs does its own HTTP, sending git's headers and the netrc's login
//! by its own rules, and nothing like this can stop it. So in the App's env
//! it isn't run at all: `lfs` refuses it.

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::error::Error;

/// Runs REAL, git's own helper, with `args` (REMOTE URL). Returns only when
/// it couldn't.
pub fn run(real: &str, args: &[String], socket: Option<&Path>) -> Error {
    let given = std::env::var("GIT_CONFIG_PARAMETERS").ok();
    match command(real, args, socket, given.as_deref()) {
        Ok(mut helper) => Error::refused(format!("{real}: {}", helper.exec())),
        Err(e) => e,
    }
}

/// REAL with `args`, for a github.com URL as above: `given` is the
/// GIT_CONFIG_PARAMETERS it would have had. Without the socket, as it is.
fn command(
    real: &str,
    args: &[String],
    socket: Option<&Path>,
    given: Option<&str>,
) -> Result<Command, Error> {
    let mut helper = Command::new(real);
    let (Some(_), [remote, url]) = (socket, args) else {
        return match socket {
            None => {
                helper.args(args);
                Ok(helper)
            }
            Some(_) => Err(Error::refused(
                "git's http helper is given its remote and its URL",
            )),
        };
    };
    match github_url(url) {
        // No App token goes over plain http, and a redirect could take the
        // user's login with it.
        Some(url)
            if url
                .get(..7)
                .is_some_and(|s| s.eq_ignore_ascii_case("http://")) =>
        {
            return Err(Error::refused(
                "acting as the App, github.com is reached over https only",
            ));
        }
        Some(url) => {
            helper
                .arg(remote)
                .arg(&url)
                .env("GIT_CONFIG_PARAMETERS", no_headers(given, &url))
                .env("NETRC", "/dev/null");
        }
        // Another host's login isn't the App's to keep from it.
        None => {
            helper.args(args);
        }
    }
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

/// A github.com URL without its user and password, read as git reads one:
/// the host ends at the first `/`, `?` or `#`, and the user's part before it
/// at the first `@`. None for another host's.
fn github_url(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("https") && !scheme.eq_ignore_ascii_case("http") {
        return None;
    }
    let (authority, path) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let host = authority.split_once('@').map_or(authority, |(_, h)| h);
    let name = host.split_once(':').map_or(host, |(n, _)| n);
    let name = name.to_ascii_lowercase();
    (name == "github.com" || name.ends_with(".github.com"))
        .then(|| format!("{scheme}://{host}{path}"))
}

/// GIT_CONFIG_PARAMETERS with an empty `http.URL.extraheader` after the rest,
/// quoted as git quotes them (`'key'='value'`).
fn no_headers(given: Option<&str>, url: &str) -> String {
    let key = format!(
        "'{}'",
        format!("http.{url}.extraheader").replace('\'', r"'\''")
    );
    match given.filter(|g| !g.trim().is_empty()) {
        Some(g) => format!("{g} {key}=''"),
        None => format!("{key}=''"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn reads_a_github_url_without_its_login() {
        for (url, bare) in [
            (
                "https://github.com/sam/r.git",
                Some("https://github.com/sam/r.git"),
            ),
            (
                "https://peter:tok@github.com/sam/r.git",
                Some("https://github.com/sam/r.git"),
            ),
            (
                "https://peter@GitHub.com:443/sam/r",
                Some("https://GitHub.com:443/sam/r"),
            ),
            (
                "HTTP://www.github.com/sam/r",
                Some("HTTP://www.github.com/sam/r"),
            ),
            ("https://github.com", Some("https://github.com")),
            // Not a login: the `@` is past the host.
            (
                "https://github.com/sam/r@x",
                Some("https://github.com/sam/r@x"),
            ),
            (
                "https://github.com?x@eve.com/",
                Some("https://github.com?x@eve.com/"),
            ),
            // Another host's, however it looks.
            ("https://gitlab.com/sam/r", None),
            ("https://github.com.eve.com/sam/r", None),
            ("https://eve.com/github.com", None),
            ("https://peter@github.com@eve.com/sam/r", None),
            ("ssh://git@github.com/sam/r", None),
            ("sam/r", None),
        ] {
            assert_eq!(github_url(url).as_deref(), bare, "{url}");
        }
    }

    #[test]
    fn clears_the_headers_last() {
        let u = "https://github.com/sam/r.git";
        assert_eq!(
            no_headers(None, u),
            "'http.https://github.com/sam/r.git.extraheader'=''"
        );
        assert_eq!(no_headers(Some(" "), u), no_headers(None, u));
        assert_eq!(
            no_headers(Some("'http.extraheader'='Authorization: x'"), u),
            "'http.extraheader'='Authorization: x' 'http.https://github.com/sam/r.git.extraheader'=''"
        );
        assert_eq!(
            no_headers(None, "https://github.com/it's"),
            r"'http.https://github.com/it'\''s.extraheader'=''"
        );
    }

    #[test]
    fn runs_git_s_helper_with_only_the_app_s_login() {
        let socket = Some(Path::new("/run/gh-token/workers.sock"));
        let run =
            |a: &[&str], socket| command("git-remote-https", &args(a), socket, Some("'a'='b'"));
        let env = |c: &Command, name: &str| {
            c.get_envs()
                .find(|(k, _)| *k == OsStr::new(name))
                .and_then(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
        };
        let parameters = |c: &Command| env(c, "GIT_CONFIG_PARAMETERS");
        let c = run(
            &["origin", "https://peter:tok@github.com/sam/r.git"],
            socket,
        )
        .unwrap();
        let given: Vec<_> = c.get_args().collect();
        assert_eq!(given, ["origin", "https://github.com/sam/r.git"]);
        assert_eq!(
            parameters(&c).as_deref(),
            Some("'a'='b' 'http.https://github.com/sam/r.git.extraheader'=''")
        );
        assert_eq!(env(&c, "NETRC").as_deref(), Some("/dev/null"));
        // Another host's, or no socket: git's, as it is.
        for (a, socket) in [
            (["origin", "https://peter:tok@gitlab.com/sam/r"], socket),
            (["origin", "https://peter:tok@github.com/sam/r"], None),
        ] {
            let c = run(&a, socket).unwrap();
            assert_eq!(c.get_args().collect::<Vec<_>>(), a, "{a:?}");
            assert_eq!(parameters(&c), None, "{a:?}");
            assert_eq!(env(&c, "NETRC"), None, "{a:?}");
        }
        // git always says which URL: refused without one.
        assert!(run(&["origin"], socket).is_err());
        // GitHub over plain http, in any spelling, isn't run as the App.
        for url in [
            "http://github.com/sam/r",
            "HTTP://GitHub.com:80/sam/r",
            "http://www.github.com/sam/r",
        ] {
            assert!(run(&["origin", url], socket).is_err(), "{url}");
            assert!(run(&["origin", url], None).is_ok(), "{url}");
        }
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
