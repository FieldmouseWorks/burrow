//! git's http and https (git-remote-http, git-remote-https), as the App's
//! env has git run them (`GIT_EXEC_PATH`): a remote, whatever its host,
//! hears only what the credential helper gives git, which is the App's
//! token for github.com and nothing for any other host. Three things git
//! would send instead, without asking the helper: the user and password in
//! the URL; the headers `http.extraHeader` adds (an Authorization of the
//! user's, say), from any config file or `-c`; and a login of the user's in
//! their netrc, which curl answers a 401 with before git sees it. So the URL
//! goes without its user and password; the headers are cleared by an empty
//! one, set last, for every URL and then for exactly that one (git takes
//! the headers of the URLs that match it most closely, and of those the
//! last one's word, and a URL git can't read matches only the first); and
//! curl's netrc (NETRC, else ~/.netrc) is an empty one.
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

/// Runs REAL, git's own helper, with `args` (REMOTE URL). Returns only when
/// it couldn't.
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
    let url = without_login(url);
    helper
        .arg(remote)
        .arg(&url)
        .env("GIT_CONFIG_PARAMETERS", no_headers(given, &url))
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

/// GIT_CONFIG_PARAMETERS with an empty `http.extraheader`, for every URL
/// and then for exactly `url`, after the rest, quoted as git quotes them
/// (`'key'='value'`).
fn no_headers(given: Option<&str>, url: &str) -> String {
    let key = format!(
        "'{}'",
        format!("http.{url}.extraheader").replace('\'', r"'\''")
    );
    let cleared = format!("'http.extraheader'='' {key}=''");
    match given.filter(|g| !g.trim().is_empty()) {
        Some(g) => format!("{g} {cleared}"),
        None => cleared,
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

    #[test]
    fn clears_the_headers_last() {
        let u = "https://github.com/sam/r.git";
        assert_eq!(
            no_headers(None, u),
            "'http.extraheader'='' 'http.https://github.com/sam/r.git.extraheader'=''"
        );
        assert_eq!(no_headers(Some(" "), u), no_headers(None, u));
        assert_eq!(
            no_headers(Some("'http.extraheader'='Authorization: x'"), u),
            "'http.extraheader'='Authorization: x' 'http.extraheader'='' 'http.https://github.com/sam/r.git.extraheader'=''"
        );
        assert_eq!(
            no_headers(None, "https://github.com/it's"),
            r"'http.extraheader'='' 'http.https://github.com/it'\''s.extraheader'=''"
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
                Some(format!(
                    "'a'='b' 'http.extraheader'='' 'http.{bare}.extraheader'=''"
                )),
                "{url}"
            );
            assert_eq!(env(&c, "NETRC").as_deref(), Some("/dev/null"), "{url}");
        }
        // No socket: git's, as it is.
        let a = ["origin", "https://peter:tok@github.com/sam/r"];
        let c = run(&a, None).unwrap();
        assert_eq!(c.get_args().collect::<Vec<_>>(), a);
        assert_eq!(parameters(&c), None);
        assert_eq!(env(&c, "NETRC"), None);
        // git always says which URL: refused without one.
        assert!(run(&["origin"], socket).is_err());
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
