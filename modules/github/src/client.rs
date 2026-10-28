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
//!   about, unless GH_TOKEN is already set.

use std::io::{self, BufRead, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use crate::error::Error;
use crate::target::{is_owner, owner_of};

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
    let got = match path.and_then(|p| owner_of(&format!("github.com/{p}"))) {
        Some(owner) => fetch(socket, &owner),
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
    if let Some(socket) = socket.filter(|_| std::env::var_os("GH_TOKEN").is_none()) {
        let owner = owner_in(args)
            .or_else(origin_owner)
            .or_else(|| std::env::var("GH_TOKEN_OWNER").ok().filter(|o| is_owner(o)))
            .or_else(|| {
                args.iter().find_map(|a| {
                    a.starts_with("https://github.com/")
                        .then(|| owner_of(a))
                        .flatten()
                })
            });
        let Some(owner) = owner else {
            return Error::refused("which account is this gh call for? Give it -R OWNER/REPO");
        };
        match fetch(socket, &owner) {
            Ok(token) => {
                run.env("GH_TOKEN", token);
            }
            Err(e) => return e,
        }
    }
    let e = run.exec();
    Error::refused(format!("{gh}: {e}"))
}

/// The account a gh call names: its `-R`/`--repo`, else an API path under
/// `repos/OWNER/`.
fn owner_in(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-R" || a == "--repo" {
            return it.next().and_then(|v| owner_of(v));
        }
        if let Some(v) = a
            .strip_prefix("--repo=")
            .or_else(|| a.strip_prefix("-R").filter(|v| !v.is_empty()))
        {
            return owner_of(v);
        }
    }
    args.iter().find_map(|a| {
        let rest = a.strip_prefix('/').unwrap_or(a).strip_prefix("repos/")?;
        let owner = rest.split('/').next()?;
        is_owner(owner).then(|| owner.to_owned())
    })
}

/// The account of the current directory's origin remote, as gh would read
/// it.
fn origin_owner() -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .output()
        .ok()?;
    out.status.success().then_some(())?;
    owner_of(String::from_utf8(out.stdout).ok()?.trim())
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
        let (_dir, path, handle) = serve(vec!["token ghs_abc\n", "refused not installed\n"]);
        let asked = "protocol=https\nhost=github.com\npath=Sam/r.git\n\n";
        assert_eq!(
            credential("get", asked, Some(&path)),
            "username=x-access-token\npassword=ghs_abc\n"
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
        assert_eq!(handle.join().unwrap(), ["Sam\n", "eve\n"]);
        // Nor when git won't say which repo.
        assert_eq!(
            credential("get", "protocol=https\nhost=github.com\n", Some(&path)),
            "quit=1\n"
        );
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
    fn finds_the_account_a_gh_call_names() {
        let args = |s: &str| s.split(' ').map(str::to_owned).collect::<Vec<_>>();
        for (call, owner) in [
            ("pr view 5 -R Sam/r", Some("Sam")),
            ("pr view 5 --repo sam/r", Some("sam")),
            ("pr view 5 --repo=github.com/sam/r", Some("sam")),
            ("pr view 5 -Rsam/r", Some("sam")),
            ("api repos/sam/r/pulls", Some("sam")),
            ("api /repos/sam/r/pulls -f body=repos/eve/x", Some("sam")),
            ("api -R eve/x repos/sam/r", Some("eve")),
            ("api graphql -f query=x", None),
            ("pr view 5", None),
            ("pr view 5 -R", None),
        ] {
            assert_eq!(owner_in(&args(call)).as_deref(), owner, "{call}");
        }
    }
}
