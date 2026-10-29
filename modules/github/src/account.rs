//! The account a gh call is about, so the token it runs with is that
//! account's installation's: the repo gh itself would act on, found the way
//! gh finds it, with the call read as gh reads it (command.rs).
//!
//! 1. What the call names: `-R`/`--repo`, `--owner` or `--org` (each of a
//!    list's values), and the arguments its usage says are a repository, an
//!    owner, an API endpoint (`repos/OWNER/...`, `orgs/OWNER/...`) or,
//!    first, a URL. A repository it reads from (`<source-repository>`) isn't
//!    one, and where the usage doesn't say what the command does with one,
//!    `ACTS_ON` does.
//! 2. GH_REPO.
//! 3. The checkout's remote gh picks: the one `gh repo set-default` chose,
//!    else upstream, github, origin, then the rest by name.
//! 4. GH_TOKEN_OWNER, for a call about no repo.
//!
//! Every other flag value and argument is the call's own data, never a name.
//! A call that names two accounts is refused, since one token can't be both,
//! and so is one for another host, by `--hostname`, GH_HOST, a URL, `-R` or
//! GH_REPO: the App's token is github.com's. Hosts are spelled as gh
//! spells them: any case, and `*.github.com` is github.com.

use std::process::Command;

use crate::command::{self, Call};
use crate::error::Error;
use crate::target::{is_owner, target_of};

/// Where gh looks besides the call, and what gh's help says of a command.
pub struct Context<'a> {
    /// GH_REPO.
    pub gh_repo: Option<String>,
    /// GH_HOST.
    pub gh_host: Option<String>,
    /// The checkout's remotes, as `git config --get-regexp` lists them.
    pub remotes: &'a dyn Fn() -> String,
    /// `gh help WORDS`.
    pub help: &'a dyn Fn(&[String]) -> Result<String, Error>,
    /// GH_TOKEN_OWNER.
    pub default: Option<String>,
}

pub fn account(args: &[String], context: &Context) -> Result<String, Error> {
    if let Some(host) = context.gh_host.as_deref().filter(|h| !h.is_empty()) {
        github_com("GH_HOST", host)?;
    }
    let call = command::read(args, context.help)?;
    let mut named: Vec<String> = Vec::new();
    for owner in named_in(&call)? {
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
        .ok_or_else(|| {
            Error::refused(format!(
                "which account is `gh {}` for? Give it -R OWNER/REPO",
                call.path.join(" ")
            ))
        })
}

/// The accounts the call itself names, by its flags and by what its usage
/// says its arguments are.
fn named_in(call: &Call) -> Result<Vec<String>, Error> {
    let mut owners = Vec::new();
    for (flag, value) in &call.values {
        match flag.as_str() {
            "repo" => owners.push(repo_owner("--repo", value)?),
            "hostname" => github_com("--hostname", value)?,
            // gh's own word for whoever's logged in.
            "owner" | "org" if value == "@me" => {}
            "owner" | "org" if is_owner(value) => owners.push(value.clone()),
            "owner" | "org" => {
                return Err(Error::refused(format!(
                    "--{flag} {value:?} isn't an account"
                )));
            }
            _ => {}
        }
    }
    for (i, (arg, names)) in call.args.iter().enumerate() {
        match (role(&call.path, i, names), url(arg)) {
            (Role::Endpoint | Role::Url, Some((host, path))) => {
                owners.extend(url_owner(host, path)?);
            }
            (Role::Endpoint, None) => owners.extend(endpoint_owner(arg)),
            (Role::Repo, _) => owners.push(repo_owner("the repository", arg)?),
            (Role::NewRepo, _) if arg.contains('/') => {
                owners.push(repo_owner("the new repository", arg)?);
            }
            (Role::Owner, _) => owners.extend(is_owner(arg).then(|| arg.clone())),
            _ => {}
        }
    }
    Ok(owners)
}

/// What an argument is to gh, by the names its usage gives its place.
#[derive(Clone, Copy)]
enum Role {
    Endpoint,
    Repo,
    /// `[HOST/][OWNER/]NAME`: a repo the call makes, in OWNER's account, or
    /// with none in the one it would act on anyway.
    NewRepo,
    Owner,
    Url,
    Data,
}

/// What a command does with an argument, where its usage's name for it
/// doesn't say: `repo create`'s `<name>` is the repo it makes, and
/// `repo fork`'s `<repository>` is read from, the fork going to `--org`.
const ACTS_ON: &[(&[&str], &str, Role)] = &[
    (&["repo", "create"], "name", Role::NewRepo),
    (&["repo", "fork"], "repository", Role::Data),
];

fn role(path: &[String], i: usize, names: &[String]) -> Role {
    let called = |is: fn(&str) -> bool| names.iter().any(|n| is(n));
    let table = ACTS_ON
        .iter()
        .find(|(p, name, _)| *p == path && names.iter().any(|n| n == name));
    if let Some(&(_, _, role)) = table {
        role
    } else if called(|n| n == "endpoint") {
        Role::Endpoint
    // What the call reads from: the labels `label clone` copies, say.
    } else if called(|n| n.starts_with("source-")) {
        Role::Data
    } else if called(|n| n == "repository" || n.ends_with("-repository") || n.ends_with("-repo")) {
        Role::Repo
    } else if called(|n| n == "owner") {
        Role::Owner
    // The first argument's URL is the one gh acts on. A later one (an
    // artifact's link, say) is data.
    } else if i == 0 && called(|n| n == "url" || n == "urls" || n.ends_with("-url")) {
        Role::Url
    } else {
        Role::Data
    }
}

/// A URL's host and path.
fn url(arg: &str) -> Option<(&str, &str)> {
    arg.strip_prefix("https://")
        .or_else(|| arg.strip_prefix("http://"))
        .map(|rest| rest.split_once('/').unwrap_or((rest, "")))
}

/// The account a github.com URL is under: api.github.com's by its endpoint.
/// Another host's is refused.
fn url_owner(host: &str, path: &str) -> Result<Option<String>, Error> {
    github_com("a URL on", host)?;
    Ok(if host.eq_ignore_ascii_case("api.github.com") {
        endpoint_owner(path)
    } else {
        target_of(&format!("github.com/{path}")).map(|t| t.owner)
    })
}

/// The account of an API path under `repos/OWNER/` or `orgs/OWNER/`.
/// `{owner}` is gh's, filled from the repo it would act on.
fn endpoint_owner(endpoint: &str) -> Option<String> {
    let path = endpoint.strip_prefix('/').unwrap_or(endpoint);
    let owner = path
        .strip_prefix("repos/")
        .or_else(|| path.strip_prefix("orgs/"))?
        .split(['/', '?'])
        .next()?;
    is_owner(owner).then(|| owner.to_owned())
}

/// The account of a `[github.com/]OWNER/REPO` or a URL, given as `what`.
fn repo_owner(what: &str, v: &str) -> Result<String, Error> {
    let target = match url(v) {
        Some((host, path)) => {
            github_com(what, host)?;
            target_of(&format!("github.com/{path}"))
        }
        None => target_of(v),
    };
    target
        .map(|t| t.owner)
        .ok_or_else(|| Error::refused(format!("{what} {v:?} isn't a repo on github.com")))
}

/// github.com, the only host the App's token is for, as gh reads a host:
/// in any case, and with any subdomain (`api.`, `www.`).
fn github_com(what: &str, host: &str) -> Result<(), Error> {
    let lower = host.to_ascii_lowercase();
    if lower == "github.com" || lower.ends_with(".github.com") {
        Ok(())
    } else {
        Err(Error::refused(format!(
            "{what} {host:?}: the App's token is for github.com"
        )))
    }
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
    use crate::command::fake::help;

    fn none() -> String {
        String::new()
    }

    /// A fork's checkout: gh acts on upstream.
    fn fork() -> String {
        "remote.origin.url git@github.com:eve/r.git\nremote.upstream.url https://github.com/sam/r.git\n"
            .into()
    }

    fn context<'a>(
        gh_repo: Option<&str>,
        remotes: &'a dyn Fn() -> String,
        default: Option<&str>,
    ) -> Context<'a> {
        Context {
            gh_repo: gh_repo.map(str::to_owned),
            gh_host: None,
            remotes,
            help: &help,
            default: default.map(str::to_owned),
        }
    }

    fn call(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn takes_what_the_call_names_first() {
        let in_fork = context(Some("eve/x"), &fork, Some("eve"));
        for (args, owner) in [
            ("pr view 5 -R Sam/r", "Sam"),
            ("pr view 5 --repo sam/r", "sam"),
            ("pr view 5 --repo=github.com/sam/r", "sam"),
            ("pr view 5 -Rsam/r", "sam"),
            ("pr view 5 -R=sam/r", "sam"),
            ("pr view 5 -wRsam/r", "sam"),
            ("pr -R sam/r view 5", "sam"),
            ("pr view https://github.com/sam/r/pull/5", "sam"),
            ("pr view https://www.GitHub.com/sam/r/pull/5", "sam"),
            ("pr checkout --force https://github.com/sam/r/pull/5", "sam"),
            (
                "co -b https://github.com/eve/x https://github.com/sam/r/pull/5",
                "sam",
            ),
            ("pr view https://github.com/sam/r/pull/5 -R Sam/r", "Sam"),
            ("api repos/sam/r/pulls", "sam"),
            ("api -- repos/sam/r", "sam"),
            ("api -X POST /repos/sam/r/pulls -f body=repos/eve/x", "sam"),
            (
                "api -X POST -F body=https://github.com/eve/x repos/sam/r/issues",
                "sam",
            ),
            ("api https://api.github.com/repos/sam/r/pulls?page=2", "sam"),
            ("api orgs/sam/members", "sam"),
            ("api --hostname github.com repos/sam/r", "sam"),
            ("api --hostname api.github.com repos/sam/r", "sam"),
            ("repo view sam/r", "sam"),
            ("repo view --json name sam/r", "sam"),
            ("repo view -b main https://github.com/sam/r", "sam"),
            ("repo clone sam/r dir", "sam"),
            ("repo clone sam/r -- https://github.com/eve/x", "sam"),
            ("repo list sam", "sam"),
            ("repo ls sam", "sam"),
            ("repo list --limit 5 sam", "sam"),
            ("secret set X --org sam", "sam"),
            ("secret set X -o sam -b https://github.com/eve/x", "sam"),
            ("issue transfer 5 sam/q -R sam/r", "sam"),
            // What the command does with them, where its usage doesn't say:
            // the repo `repo create` makes, the account a fork goes to, the
            // repo labels are cloned into rather than from.
            ("repo create sam/new --private", "sam"),
            ("repo new Sam/new", "Sam"),
            ("repo create github.com/sam/new", "sam"),
            ("repo create --template pat/t sam/new", "sam"),
            ("repo fork pat/x --org sam", "sam"),
            ("label clone pat/x -R sam/r", "sam"),
            // A list's every value, however given.
            ("search issues --repo sam/r,Sam/q", "sam"),
            ("search issues --repo sam/r --repo sam/q", "sam"),
            ("search issues --owner sam,Sam", "sam"),
            (r#"search issues --owner "sam" -R sam/r"#, "sam"),
            // A flag's value is data, wherever the flag is and however given.
            (
                "issue -R sam/r comment 5 --body https://github.com/eve/x",
                "sam",
            ),
            (
                "issue comment 5 -R sam/r -b https://github.com/eve/x",
                "sam",
            ),
            (
                "issue comment 5 -R sam/r --body=https://github.com/eve/x",
                "sam",
            ),
            ("issue comment 5 -R sam/r -bhttps://github.com/eve/x", "sam"),
            ("issue comment -b --repo=eve/x 5 -R sam/r", "sam"),
            (
                "issue comment https://github.com/sam/r/issues/5 -F https://github.com/eve/x",
                "sam",
            ),
        ] {
            assert_eq!(account(&call(args), &in_fork).unwrap(), owner, "{args}");
        }
    }

    #[test]
    fn refuses_two_accounts_or_another_host() {
        // Refused for what the call says, though gh's checkout has an account.
        let ctx = context(None, &fork, Some("sam"));
        for args in [
            "pr view https://github.com/sam/r/pull/5 -R eve/x",
            "secret set X --org eve -R sam/r",
            "issue transfer 5 eve/x -R sam/r",
            "repo create ghe.example.com/sam/new",
            "repo create https://ghe.example.com/sam/new",
            "search issues --repo sam/r,eve/x",
            "search issues --owner sam,eve",
            "search issues --owner sam --repo eve/x",
            // Not an account, where only one goes.
            "search issues --owner sam,not!one",
            "secret set X --org not!one",
            "project list --owner not!one",
            "pr view 5 -R ghe.example.com/sam/r",
            "pr view 5 -R sam",
            "pr view 5 -R",
            "api --hostname ghe.example.com repos/sam/r",
            "api --hostname=ghe.example.com repos/sam/r",
            "api --hostname github.com.example repos/sam/r",
            "pr view https://sam.ghe.com/sam/r/pull/5",
            "pr view https://gitlab.com/eve/x",
            "api https://ghe.example.com/api/v3/repos/sam/r",
            "repo clone https://ghe.example.com/sam/r",
            // gh can't say what it is, or would refuse it itself.
            "nosuch 5",
            "pr view 5 --nosuch",
        ] {
            assert!(account(&call(args), &ctx).is_err(), "{args}");
        }
        assert!(
            account(
                &call("pr view 5"),
                &context(Some("ghe.example.com/sam/r"), &fork, None)
            )
            .is_err()
        );
        let on_ghe = Context {
            gh_host: Some("ghe.example.com".into()),
            ..context(None, &fork, None)
        };
        assert!(account(&call("pr view 5 -R sam/r"), &on_ghe).is_err());
        let on_github = Context {
            gh_host: Some("GitHub.com".into()),
            ..context(None, &fork, None)
        };
        assert_eq!(account(&call("pr view 5"), &on_github).unwrap(), "sam");
    }

    #[test]
    fn then_gh_repo_then_the_remote_gh_picks_then_the_default() {
        let args = call("pr view 5");
        assert_eq!(
            account(&args, &context(Some("sam/r"), &fork, Some("eve"))).unwrap(),
            "sam"
        );
        // An empty GH_REPO is none, as gh reads it.
        assert_eq!(
            account(&args, &context(Some(""), &fork, None)).unwrap(),
            "sam"
        );
        assert_eq!(
            account(&args, &context(None, &fork, Some("eve"))).unwrap(),
            "sam"
        );
        // Not a name: a branch, `{owner}`, a query, a file's path, a flag's
        // value, another host's URL where it's data.
        for args in [
            "pr view feature/x",
            "api repos/{owner}/{repo}/pulls",
            "api graphql -f query=x",
            "search issues https://github.com/eve/x",
            "repo read-file docs/x.md",
            "pr view 5 --json https://github.com/eve/x",
            "issue comment 5 --body https://gitlab.com/eve/x",
            "issue artifact create 5 https://gitlab.com/eve/x",
            // What a call reads from, or makes without saying where.
            "label clone pat/x",
            "repo fork pat/x",
            "repo create new",
            // gh's own word for whoever's logged in.
            "project list --owner @me",
        ] {
            assert_eq!(
                account(&call(args), &context(None, &fork, None)).unwrap(),
                "sam",
                "{args}"
            );
        }
        assert_eq!(
            account(&args, &context(None, &none, Some("sam"))).unwrap(),
            "sam"
        );
        assert!(account(&args, &context(None, &none, None)).is_err());
        assert!(account(&args, &context(None, &none, Some("not an owner"))).is_err());
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
