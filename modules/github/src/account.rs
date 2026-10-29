//! The account a gh call is about, so the token it runs with is that
//! account's installation's: the repo gh itself would act on, found the way
//! gh finds it.
//!
//! 1. What the call names: a github.com URL among its arguments, `-R` or
//!    `--repo`, `gh api`'s `repos/OWNER/` or `orgs/OWNER/` endpoint, or
//!    `gh repo`'s OWNER/REPO.
//! 2. GH_REPO.
//! 3. The checkout's remote gh picks: the one `gh repo set-default` chose,
//!    else upstream, github, origin, then the rest by name.
//! 4. GH_TOKEN_OWNER, for a call about no repo.
//!
//! A call that names two accounts is refused: one token can't be both. A
//! repo on another host is refused too, since the App's token is for
//! github.com. A flag's `--flag=VALUE` is never read as a name, but a value
//! given as `--flag VALUE` can't be told from an argument without gh's own
//! tables: one that is exactly another account's URL gets that account's
//! token, and gh's call fails. It never gets the user's own login.

use std::process::Command;

use crate::error::Error;
use crate::target::{is_owner, target_of};

/// Where gh looks when the call names no repo.
pub struct Context {
    /// GH_REPO.
    pub gh_repo: Option<String>,
    /// The checkout's remotes, as `git config --get-regexp` lists them.
    pub remotes: fn() -> String,
    /// GH_TOKEN_OWNER.
    pub default: Option<String>,
}

pub fn account(args: &[String], context: &Context) -> Result<String, Error> {
    let mut named: Vec<String> = Vec::new();
    for owner in named_in(args)? {
        if !named.iter().any(|n| n.eq_ignore_ascii_case(&owner)) {
            named.push(owner);
        }
    }
    match named.as_slice() {
        [one] => return Ok(one.clone()),
        [] => {}
        [a, b, ..] => {
            return Err(Error::refused(format!(
                "this gh call names {a} and {b}, and one token can't be for both"
            )));
        }
    }
    if let Some(r) = context.gh_repo.as_deref().filter(|r| !r.is_empty()) {
        return repo_owner("GH_REPO", r);
    }
    if let Some(owner) = picked_remote(&(context.remotes)()) {
        return Ok(owner);
    }
    context
        .default
        .clone()
        .filter(|o| is_owner(o))
        .ok_or_else(|| Error::refused("which account is this gh call for? Give it -R OWNER/REPO"))
}

/// The accounts the call itself names.
fn named_in(args: &[String]) -> Result<Vec<String>, Error> {
    let mut owners = Vec::new();
    // Arguments and the values of flags given apart: see the top.
    let mut words = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--" {
            words.extend(it.by_ref());
        } else if a == "-R" || a == "--repo" {
            let v = it
                .next()
                .ok_or_else(|| Error::refused(format!("{a} is OWNER/REPO")))?;
            owners.push(repo_owner(a, v)?);
        } else if let Some(v) = a.strip_prefix("--repo=") {
            owners.push(repo_owner("--repo", v)?);
        } else if let Some(v) = a.strip_prefix("-R").filter(|v| !v.is_empty()) {
            owners.push(repo_owner("-R", v)?);
        } else if !a.starts_with('-') {
            words.push(a);
        }
    }
    owners.extend(
        words
            .iter()
            .filter(|w| w.starts_with("https://github.com/") || w.starts_with("http://github.com/"))
            .filter_map(|w| target_of(w))
            .map(|t| t.owner),
    );
    match words.as_slice() {
        [api, rest @ ..] if *api == "api" => owners.extend(rest.iter().filter_map(|w| {
            let path = w.strip_prefix('/').unwrap_or(w);
            let owner = path
                .strip_prefix("repos/")
                .or_else(|| path.strip_prefix("orgs/"))?
                .split('/')
                .next()?;
            // `{owner}` is gh's, filled from the repo it would act on.
            is_owner(owner).then(|| owner.to_owned())
        })),
        [repo, _, rest @ ..] if *repo == "repo" => {
            owners.extend(rest.iter().find_map(|w| target_of(w)).map(|t| t.owner));
        }
        _ => {}
    }
    Ok(owners)
}

/// The account of a `[github.com/]OWNER/REPO` given as `what`.
fn repo_owner(what: &str, v: &str) -> Result<String, Error> {
    target_of(v)
        .map(|t| t.owner)
        .ok_or_else(|| Error::refused(format!("{what} {v:?} isn't a repo on github.com")))
}

/// The account of the remote gh would pick, from
/// `git config --get-regexp '^remote\..*\.(url|gh-resolved)$'`: the one
/// `gh repo set-default` marked (`base`, or `base:OWNER/REPO`), else the
/// first of the github.com remotes by gh's order.
fn picked_remote(config: &str) -> Option<String> {
    #[derive(Default)]
    struct Remote {
        name: String,
        url: Option<String>,
        resolved: Option<String>,
    }
    let mut remotes: Vec<Remote> = Vec::new();
    for line in config.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        let Some((name, field)) = key.strip_prefix("remote.").and_then(|k| k.rsplit_once('.'))
        else {
            continue;
        };
        let i = match remotes.iter().position(|r| r.name == name) {
            Some(i) => i,
            None => {
                remotes.push(Remote {
                    name: name.to_owned(),
                    ..Remote::default()
                });
                remotes.len() - 1
            }
        };
        match field {
            "url" => remotes[i].url = Some(value.to_owned()),
            "gh-resolved" => remotes[i].resolved = Some(value.to_owned()),
            _ => {}
        }
    }
    let rank = |name: &str| match name {
        "upstream" => 0,
        "github" => 1,
        "origin" => 2,
        _ => 3,
    };
    let owner = |url: &Option<String>| url.as_deref().and_then(target_of).map(|t| t.owner);
    remotes.retain(|r| owner(&r.url).is_some());
    remotes.sort_by(|a, b| {
        rank(&a.name)
            .cmp(&rank(&b.name))
            .then_with(|| a.name.cmp(&b.name))
    });
    for r in &remotes {
        match r.resolved.as_deref() {
            Some("base") => return owner(&r.url),
            Some(chosen) => {
                if let Some(full) = chosen.strip_prefix("base:") {
                    return target_of(full).map(|t| t.owner);
                }
            }
            None => {}
        }
    }
    remotes.first().and_then(|r| owner(&r.url))
}

/// The current directory's remotes, for picked_remote: nothing outside a
/// checkout.
pub fn remotes() -> String {
    Command::new("git")
        .args(["config", "--get-regexp", r"^remote\..*\.(url|gh-resolved)$"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(gh_repo: Option<&str>, remotes: fn() -> String, default: Option<&str>) -> Context {
        Context {
            gh_repo: gh_repo.map(str::to_owned),
            remotes,
            default: default.map(str::to_owned),
        }
    }

    fn none() -> String {
        String::new()
    }

    /// A fork's checkout: gh acts on upstream.
    fn fork() -> String {
        "remote.origin.url git@github.com:eve/r.git\nremote.upstream.url https://github.com/sam/r.git\n"
            .into()
    }

    fn call(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn takes_what_the_call_names_first() {
        let in_fork = context(Some("eve/x"), fork, Some("eve"));
        for (args, owner) in [
            ("pr view 5 -R Sam/r", "Sam"),
            ("pr view 5 --repo sam/r", "sam"),
            ("pr view 5 --repo=github.com/sam/r", "sam"),
            ("pr view 5 -Rsam/r", "sam"),
            ("pr view https://github.com/sam/r/pull/5", "sam"),
            ("pr checkout --force https://github.com/sam/r/pull/5", "sam"),
            ("pr view https://github.com/sam/r/pull/5 -R Sam/r", "Sam"),
            ("api repos/sam/r/pulls", "sam"),
            ("api -X POST /repos/sam/r/pulls -f body=repos/eve/x", "sam"),
            ("api orgs/sam/members", "sam"),
            ("api -R sam/x repos/{owner}/{repo}/pulls", "sam"),
            ("repo view sam/r", "sam"),
            ("repo clone sam/r dir", "sam"),
            ("repo view --json name sam/r", "sam"),
            ("pr create --body=https://github.com/sam/r -R sam/r", "sam"),
        ] {
            assert_eq!(account(&call(args), &in_fork).unwrap(), owner, "{args}");
        }
    }

    #[test]
    fn refuses_two_accounts_or_another_host() {
        // Refused for what the call says, though gh's checkout has an account.
        let ctx = context(None, fork, Some("sam"));
        for args in [
            "pr view https://github.com/sam/r/pull/5 -R eve/x",
            "api repos/sam/r -R eve/x",
            "pr view 5 -R ghe.example.com/sam/r",
            "pr view 5 -R sam",
            "pr view 5 -R",
        ] {
            assert!(account(&call(args), &ctx).is_err(), "{args}");
        }
        assert!(
            account(
                &call("pr view 5"),
                &context(Some("ghe.example.com/sam/r"), fork, None)
            )
            .is_err()
        );
    }

    #[test]
    fn then_gh_repo_then_the_remote_gh_picks_then_the_default() {
        let args = call("pr view 5");
        assert_eq!(
            account(&args, &context(Some("sam/r"), fork, Some("eve"))).unwrap(),
            "sam"
        );
        // An empty GH_REPO is none, as gh reads it.
        assert_eq!(
            account(&args, &context(Some(""), fork, None)).unwrap(),
            "sam"
        );
        assert_eq!(
            account(&args, &context(None, fork, Some("eve"))).unwrap(),
            "sam"
        );
        // Not a URL or a remote, so not a name: a branch, a number, `{owner}`.
        for args in [
            "pr view feature/x",
            "api repos/{owner}/{repo}/pulls",
            "api graphql -f query=x",
            "pr view 5 --body https://gitlab.com/eve/x",
        ] {
            assert_eq!(
                account(&call(args), &context(None, fork, None)).unwrap(),
                "sam",
                "{args}"
            );
        }
        assert_eq!(
            account(&args, &context(None, none, Some("sam"))).unwrap(),
            "sam"
        );
        assert!(account(&args, &context(None, none, None)).is_err());
        assert!(account(&args, &context(None, none, Some("not an owner"))).is_err());
    }

    #[test]
    fn picks_the_remote_gh_would() {
        for (config, owner) in [
            ("remote.origin.url https://github.com/sam/r\n", Some("sam")),
            (&fork() as &str, Some("sam")),
            (
                "remote.origin.url https://github.com/sam/r\nremote.github.url https://github.com/eve/r\n",
                Some("eve"),
            ),
            (
                "remote.b.url https://github.com/eve/r\nremote.a.url https://github.com/sam/r\n",
                Some("sam"),
            ),
            // set-default's choice, either way it's written.
            (
                &(fork() + "remote.origin.gh-resolved base\n") as &str,
                Some("eve"),
            ),
            (
                &(fork() + "remote.upstream.gh-resolved base:pat/r\n") as &str,
                Some("pat"),
            ),
            // Another host's remote is passed over; a dotted name is one name.
            (
                "remote.upstream.url https://gitlab.com/eve/r\nremote.my.fork.url https://github.com/sam/r\n",
                Some("sam"),
            ),
            ("remote.origin.url /srv/git/r\n", None),
            ("", None),
        ] {
            assert_eq!(picked_remote(config).as_deref(), owner, "{config}");
        }
    }
}
