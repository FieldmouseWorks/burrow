//! The askers' side. Each asks the socket GH_TOKEN_SOCKET names, and only
//! when it's set: without it nothing here acts, and gh and git go on with
//! the user's own login as before.
//!
//! - `get TARGET`: prints a token.
//! - `git-credential OP`: git's credential helper, for the repo git names
//!   (credential.useHttpPath). It's asked before gh's login and, with the
//!   socket set, gives an answer or stops git: it never lets git fall back
//!   to the user's own login.
//! - `gh GH ARGS...`: runs GH with a token for the account the call is
//!   about (account.rs), unless it's given one already in GH_TOKEN.

use std::ffi::OsStr;
use std::io::{self, BufRead, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::account::{self, Context};
use crate::error::Error;
use crate::target::target_of;

/// The longest answer read: a token is shorter, and so is GitHub's no.
const MAX_ANSWER: u64 = 4096;

/// A token for `target` (`OWNER` or `OWNER/REPO`) from the socket.
pub fn fetch(socket: &Path, target: &str) -> Result<String, Error> {
    let at = |e: io::Error| Error::unavailable(format!("{}: {e}", socket.display()));
    let mut s = UnixStream::connect(socket).map_err(at)?;
    // A mint is two calls to GitHub, each given thirty seconds.
    s.set_read_timeout(Some(Duration::from_secs(75)))
        .map_err(at)?;
    s.write_all(format!("{target}\n").as_bytes()).map_err(at)?;
    s.shutdown(Shutdown::Write).map_err(at)?;
    let mut reply = String::new();
    s.take(MAX_ANSWER).read_to_string(&mut reply).map_err(at)?;
    match reply.strip_suffix('\n').and_then(|r| r.split_once(' ')) {
        Some(("token", t)) if !t.is_empty() => Ok(t.to_owned()),
        Some(("unavailable", why)) => Err(Error::unavailable(why)),
        Some(("refused", why)) => Err(Error::refused(why)),
        // The service stopped partway: asking again can fix that.
        _ => Err(Error::unavailable(format!(
            "{}: no answer",
            socket.display()
        ))),
    }
}

/// git's credential helper: `op` is get, store or erase.
pub fn git_credential(
    op: &str,
    input: impl BufRead,
    mut output: impl Write,
    socket: Option<&Path>,
) -> io::Result<()> {
    let Some(socket) = socket else {
        return Ok(());
    };
    // Nothing's kept: a store or erase is the next helper's.
    if op != "get" {
        return Ok(());
    }
    let (mut protocol, mut host, mut path) = (None, None, None);
    for line in input.lines() {
        let line = line?;
        if line.is_empty() {
            break;
        }
        match line.split_once('=') {
            Some(("protocol", v)) => protocol = Some(v.to_owned()),
            Some(("host", v)) => host = Some(v.to_owned()),
            Some(("path", v)) => path = Some(v.to_owned()),
            _ => {}
        }
    }
    // Another host's login isn't the App's to give.
    if protocol.as_deref() != Some("https") || host.as_deref() != Some("github.com") {
        return Ok(());
    }
    // That repo's token, not its account's.
    let got = match path.and_then(|p| target_of(&format!("github.com/{p}"))) {
        Some(repo) => fetch(socket, &repo.to_string()),
        None => Err(Error::refused(
            "git didn't say which repo (credential.useHttpPath)",
        )),
    };
    match got {
        Ok(token) => writeln!(output, "username=x-access-token\npassword={token}"),
        Err(e) => {
            eprintln!("gh-token: no token for git: {e}");
            // Stops git here, rather than its next helper's login.
            writeln!(output, "quit=1")
        }
    }
}

/// Runs `gh` with `args` and a token for the account they're about.
/// Returns only when it couldn't.
pub fn gh(gh: &str, args: &[String], socket: Option<&Path>) -> Error {
    let mut run = Command::new(gh);
    run.args(args);
    // `gh help`, not `--help`: an extension given --help would run.
    let help = |words: &[String]| -> Result<String, Error> {
        let out = Command::new(gh)
            .arg("help")
            .args(words)
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .output()
            .map_err(|e| Error::refused(format!("{gh} help: {e}")))?;
        let help = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() || help.is_empty() {
            return Err(Error::refused(format!(
                "gh doesn't know `gh {}`",
                words.join(" ")
            )));
        }
        Ok(help)
    };
    let context = Context {
        gh_repo: std::env::var("GH_REPO").ok(),
        gh_host: std::env::var("GH_HOST").ok(),
        remotes: &account::remotes,
        help: &help,
        default: std::env::var("GH_TOKEN_OWNER").ok(),
    };
    match token_for(
        args,
        std::env::var_os("GH_TOKEN").as_deref(),
        socket,
        &context,
    ) {
        Ok(Some(token)) => {
            run.env("GH_TOKEN", token);
        }
        Ok(None) => {}
        Err(e) => return e,
    }
    let e = run.exec();
    Error::refused(format!("{gh}: {e}"))
}

/// The App's token for the call, when there's a socket to ask and gh isn't
/// given one already. An empty GH_TOKEN is none: gh reads it so, and would
/// go on to GITHUB_TOKEN.
fn token_for(
    args: &[String],
    given: Option<&OsStr>,
    socket: Option<&Path>,
    context: &Context,
) -> Result<Option<String>, Error> {
    match socket {
        Some(socket) if given.is_none_or(OsStr::is_empty) => {
            fetch(socket, &account::account(args, context)?).map(Some)
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::thread;

    /// A socket that answers each connection with the next of `replies`,
    /// and the requests it got.
    fn serve(
        replies: Vec<&'static str>,
    ) -> (tempfile::TempDir, PathBuf, thread::JoinHandle<Vec<String>>) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sock");
        let listener = UnixListener::bind(&path).unwrap();
        let handle = thread::spawn(move || {
            let mut asked = Vec::new();
            for reply in replies {
                let (mut s, _) = listener.accept().unwrap();
                let mut request = String::new();
                s.read_to_string(&mut request).unwrap();
                asked.push(request);
                s.write_all(reply.as_bytes()).unwrap();
            }
            asked
        });
        (dir, path, handle)
    }

    #[test]
    fn fetches_a_token_or_says_why_not() {
        let (_dir, path, handle) = serve(vec![
            "token ghs_abc\n",
            "unavailable GitHub: 502\n",
            "refused the App isn't installed on eve\n",
            "",
            "token \n",
        ]);
        assert_eq!(fetch(&path, "sam/r").unwrap(), "ghs_abc");
        assert_eq!(
            fetch(&path, "sam").unwrap_err(),
            Error::unavailable("GitHub: 502")
        );
        assert_eq!(
            fetch(&path, "eve").unwrap_err(),
            Error::refused("the App isn't installed on eve")
        );
        assert!(fetch(&path, "sam").unwrap_err().unavailable);
        assert!(fetch(&path, "sam").unwrap_err().unavailable);
        assert_eq!(
            handle.join().unwrap(),
            ["sam/r\n", "sam\n", "eve\n", "sam\n", "sam\n"]
        );
        // No service at all: asking later can fix that.
        assert!(
            fetch(&_dir.path().join("none"), "sam")
                .unwrap_err()
                .unavailable
        );
    }

    fn credential(op: &str, input: &str, socket: Option<&Path>) -> String {
        let mut out = Vec::new();
        git_credential(op, input.as_bytes(), &mut out, socket).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn gives_git_the_token_for_the_repo_it_names() {
        let (_dir, path, handle) = serve(vec![
            "token ghs_abc\n",
            "token ghs_lfs\n",
            "refused not installed\n",
        ]);
        let asked = "protocol=https\nhost=github.com\npath=Sam/r.git\n\n";
        assert_eq!(
            credential("get", asked, Some(&path)),
            "username=x-access-token\npassword=ghs_abc\n"
        );
        assert_eq!(
            credential(
                "get",
                "protocol=https\nhost=github.com\npath=sam/r.git/info/lfs\n",
                Some(&path)
            ),
            "username=x-access-token\npassword=ghs_lfs\n"
        );
        // A failure stops git: no falling back to the user's own login.
        assert_eq!(
            credential(
                "get",
                "protocol=https\nhost=github.com\npath=eve/x\n",
                Some(&path)
            ),
            "quit=1\n"
        );
        // Each the repo's own token, never the account's.
        assert_eq!(handle.join().unwrap(), ["Sam/r\n", "sam/r\n", "eve/x\n"]);
        // Nor when git won't say which repo, or what it says isn't one.
        for no_repo in ["", "path=sam\n", "path=sam/..\n"] {
            assert_eq!(
                credential(
                    "get",
                    &format!("protocol=https\nhost=github.com\n{no_repo}"),
                    Some(&path)
                ),
                "quit=1\n",
                "{no_repo:?}"
            );
        }
        // Not GitHub, not https, not a get, or no socket: the next helper's.
        assert_eq!(
            credential(
                "get",
                "protocol=https\nhost=gitlab.com\npath=sam/r\n",
                Some(&path)
            ),
            ""
        );
        assert_eq!(
            credential(
                "get",
                "protocol=http\nhost=github.com\npath=sam/r\n",
                Some(&path)
            ),
            ""
        );
        assert_eq!(credential("store", asked, Some(&path)), "");
        assert_eq!(credential("get", asked, None), "");
    }

    #[test]
    fn gives_gh_the_token_for_its_account_unless_it_has_one() {
        let (_dir, path, handle) = serve(vec!["token ghs_abc\n", "token ghs_def\n"]);
        let help = |_: &[String]| {
            Ok("FLAGS\n  -w, --web   Open in the browser\n\nINHERITED FLAGS\n  -R, --repo [HOST/]OWNER/REPO   Select another repository\n".to_owned())
        };
        let context = Context {
            gh_repo: None,
            gh_host: None,
            remotes: &String::new,
            help: &help,
            default: None,
        };
        let args: Vec<String> = ["pr", "view", "5", "-R", "sam/r"].map(String::from).into();
        let socket = Some(path.as_path());
        assert_eq!(
            token_for(&args, None, socket, &context).unwrap().as_deref(),
            Some("ghs_abc")
        );
        // An empty one is none.
        assert_eq!(
            token_for(&args, Some(OsStr::new("")), socket, &context)
                .unwrap()
                .as_deref(),
            Some("ghs_def")
        );
        assert_eq!(handle.join().unwrap(), ["sam\n", "sam\n"]);
        // One given, or no socket: gh's as it is.
        assert_eq!(
            token_for(&args, Some(OsStr::new("mine")), socket, &context).unwrap(),
            None
        );
        assert_eq!(token_for(&args, None, None, &context).unwrap(), None);
        // No account to be had: refused, the socket never asked.
        let nothing: Vec<String> = ["api", "user"].map(String::from).into();
        assert!(token_for(&nothing, None, socket, &context).is_err());
    }
}
