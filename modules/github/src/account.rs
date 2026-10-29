//! The account a gh call is about, so the token it runs with is that
//! account's installation's: the repo gh itself would act on, found the way
//! gh finds it.
//!
//! 1. What the call names: `-R`/`--repo`, `--owner` or `--org`, a
//!    github.com URL among its arguments, `gh api`'s `repos/OWNER/` or
//!    `orgs/OWNER/` endpoint, or `gh repo`'s OWNER/REPO (`gh repo list`'s
//!    OWNER).
//! 2. GH_REPO.
//! 3. The checkout's remote gh picks: the one `gh repo set-default` chose,
//!    else upstream, github, origin, then the rest by name.
//! 4. GH_TOKEN_OWNER, for a call about no repo.
//!
//! A flag's value is never read as a name, unless the flag is one of those:
//! which flags take a value is what gh's own help for the command says.
//! A call that names two accounts is refused, since one token can't be both,
//! and so is one for another host, by `--hostname`, GH_HOST, a URL, `-R` or
//! GH_REPO: the App's token is github.com's. Hosts are spelled as gh
//! spells them: any case, and `*.github.com` is github.com.

use std::collections::HashMap;
use std::process::Command;

use crate::error::Error;
use crate::target::{is_owner, target_of};

/// Where gh looks besides the call, and what gh says the call's flags are.
pub struct Context<'a> {
    /// GH_REPO.
    pub gh_repo: Option<String>,
    /// GH_HOST.
    pub gh_host: Option<String>,
    /// The checkout's remotes, as `git config --get-regexp` lists them.
    pub remotes: &'a dyn Fn() -> String,
    /// `gh help WORDS`, for the command WORDS begin.
    pub help: &'a dyn Fn(&[String]) -> Result<String, Error>,
    /// GH_TOKEN_OWNER.
    pub default: Option<String>,
}

pub fn account(args: &[String], context: &Context) -> Result<String, Error> {
    if let Some(host) = context.gh_host.as_deref().filter(|h| !h.is_empty()) {
        github_com("GH_HOST", host)?;
    }
    let command: Vec<String> = args
        .iter()
        .take_while(|a| !a.starts_with('-'))
        .cloned()
        .collect();
    let flags = Flags::of((context.help)(&command)?.as_str());
    let mut named: Vec<String> = Vec::new();
    for owner in named_in(args, &flags)? {
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

/// A command's flags that take a value, by long name, and their short
/// names, from its help's FLAGS sections:
/// `  -b, --body text   The comment body text` takes one, and
/// `  -w, --web         Open the web browser` doesn't.
#[derive(Default)]
struct Flags {
    long: Vec<String>,
    short: HashMap<char, String>,
}

impl Flags {
    fn of(help: &str) -> Self {
        let mut flags = Self::default();
        let mut in_flags = false;
        for line in help.lines().filter(|l| !l.is_empty()) {
            if !line.starts_with(' ') {
                in_flags = line.ends_with("FLAGS");
                continue;
            }
            let spec = line.trim_start();
            if !in_flags || !spec.starts_with('-') {
                continue;
            }
            let spec = spec.split("  ").next().unwrap_or_default();
            let (short, long) = match spec.split_once(", ") {
                Some((s, l)) => (s.strip_prefix('-').and_then(|s| s.chars().next()), l),
                None => (None, spec),
            };
            let Some((name, _value)) = long.strip_prefix("--").and_then(|l| l.split_once(' '))
            else {
                continue;
            };
            if let Some(c) = short {
                flags.short.insert(c, name.to_owned());
            }
            flags.long.push(name.to_owned());
        }
        flags
    }
}

/// The accounts the call itself names.
fn named_in(args: &[String], flags: &Flags) -> Result<Vec<String>, Error> {
    let mut owners = Vec::new();
    let mut words = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        // What follows is passed on, as `gh repo clone`'s to git.
        if a == "--" {
            break;
        }
        let (name, attached) = if let Some(long) = a.strip_prefix("--") {
            match long.split_once('=') {
                Some((n, v)) => (n.to_owned(), Some(v.to_owned())),
                None => (long.to_owned(), None),
            }
        } else if let Some(cluster) = a.strip_prefix('-').filter(|c| !c.is_empty()) {
            // Short flags run together: the first that takes a value takes
            // the rest of the word, or the next one.
            let Some((i, name)) = cluster
                .char_indices()
                .find_map(|(i, c)| flags.short.get(&c).map(|n| (i + c.len_utf8(), n)))
            else {
                continue;
            };
            let rest = &cluster[i..];
            (name.clone(), (!rest.is_empty()).then(|| rest.to_owned()))
        } else {
            words.push(a.as_str());
            continue;
        };
        if !flags.long.contains(&name) {
            continue;
        }
        let Some(value) = attached.or_else(|| it.next().cloned()) else {
            return Err(Error::refused(format!("--{name} wants a value")));
        };
        match name.as_str() {
            "repo" => owners.push(repo_owner("--repo", &value)?),
            "hostname" => github_com("--hostname", &value)?,
            "owner" | "org" if is_owner(&value) => owners.push(value),
            _ => {}
        }
    }
    for w in &words {
        let Some((host, path)) = w
            .strip_prefix("https://")
            .or_else(|| w.strip_prefix("http://"))
            .map(|rest| rest.split_once('/').unwrap_or((rest, "")))
        else {
            continue;
        };
        github_com("a URL on", host)?;
        if host.eq_ignore_ascii_case("api.github.com") {
            owners.extend(endpoint_owner(path));
        } else {
            owners.extend(target_of(&format!("github.com/{path}")).map(|t| t.owner));
        }
    }
    match words.as_slice() {
        ["api", endpoint, ..] => owners.extend(endpoint_owner(endpoint)),
        ["repo", "list", owner, ..] if is_owner(owner) => owners.push(owner.to_string()),
        ["repo", sub, repo, ..] if *sub != "list" => {
            owners.extend(target_of(repo).map(|t| t.owner));
        }
        _ => {}
    }
    Ok(owners)
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

/// The account of a `[github.com/]OWNER/REPO` given as `what`.
fn repo_owner(what: &str, v: &str) -> Result<String, Error> {
    target_of(v)
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

    /// What gh's help says of the commands the tests call, as gh prints it.
    fn help(command: &[String]) -> Result<String, Error> {
        let repo = "\nINHERITED FLAGS\n      --help                     Show help for command\n  -R, --repo [HOST/]OWNER/REPO   Select another repository using the [HOST/]OWNER/REPO format\n";
        let flags = match command.get(..2).unwrap_or(command) {
            [a, b] if a == "pr" && b == "view" => {
                "  -c, --comments          View pull request comments\n  -q, --jq expression     Filter JSON output using a jq expression\n      --json fields       Output JSON with the specified fields\n  -w, --web               Open a pull request in the browser\n"
            }
            [a, b] if a == "pr" && b == "checkout" => {
                "  -b, --branch string        Local branch name to use (default [the name of the head branch])\n  -f, --force                Reset the existing local branch to the latest state of the pull request\n"
            }
            [a, b] if a == "issue" && b == "comment" => {
                "  -b, --body text        The comment body text\n  -F, --body-file file   Read body text from file (use \"-\" to read from standard input)\n  -e, --editor           Skip prompts and open the text editor to write the body in\n  -w, --web              Open the web browser to write the comment\n"
            }
            [a, b] if a == "secret" && b == "set" => {
                "  -b, --body string          The value for the secret (reads from standard input if not specified)\n  -o, --org organization     Set organization secret\n  -u, --user                 Set a secret for your user\n"
            }
            [a, ..] if a == "api" => {
                return Ok("FLAGS\n  -F, --field key=value          Add a typed parameter in key=value format\n      --hostname string          The GitHub hostname for the request (default \"github.com\")\n  -i, --include                  Include HTTP response status line and headers in the output\n  -q, --jq string                Query to select values from the response using jq syntax\n  -X, --method string            The HTTP method for the request (default \"GET\")\n      --paginate                 Make additional HTTP requests to fetch all pages of results\n  -f, --raw-field key=value      Add a string parameter in key=value format\n\nINHERITED FLAGS\n  --help   Show help for command\n\nEXAMPLES\n  $ gh api -X GET search/issues -f q='repo:cli/cli is:open remote'\n".into());
            }
            [a, b] if a == "repo" && b == "list" => {
                return Ok("FLAGS\n  -q, --jq expression       Filter JSON output using a jq expression\n  -L, --limit int           Maximum number of repositories to list (default 30)\n      --source              Show only non-forks\n  --help   Show help for command\n".into());
            }
            [a, _] if a == "repo" => {
                return Ok("FLAGS\n  -b, --branch string     View a specific branch of the repository\n      --json fields       Output JSON with the specified fields\n  -w, --web               Open a repository in the browser\n".into());
            }
            _ => return Err(Error::refused(format!("gh {command:?} --help: unknown"))),
        };
        Ok(format!(
            "USAGE\n  gh {}\n\nFLAGS\n{flags}{repo}",
            command.join(" ")
        ))
    }

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
    fn reads_which_flags_take_a_value_from_the_help() {
        let flags = Flags::of(&help(&call("issue comment")).unwrap());
        assert_eq!(flags.long, ["body", "body-file", "repo"]);
        assert_eq!(flags.short.get(&'b').map(String::as_str), Some("body"));
        assert_eq!(flags.short.get(&'R').map(String::as_str), Some("repo"));
        assert_eq!(flags.short.get(&'w'), None);
        // A blank line in a section doesn't end it.
        assert_eq!(
            Flags::of("FLAGS\n  -a, --aa x   y\n\n  -b, --bb x   y\n").long,
            ["aa", "bb"]
        );
        // Only under FLAGS: not an example, and not a help without any.
        let api = Flags::of(&help(&call("api")).unwrap());
        assert!(
            api.long.contains(&"method".to_owned()) && !api.long.contains(&"paginate".to_owned())
        );
        assert!(
            Flags::of("USAGE\n  gh x\n\nEXAMPLES\n  -b, --body text   x\n")
                .long
                .is_empty()
        );
    }

    #[test]
    fn takes_what_the_call_names_first() {
        let in_fork = context(Some("eve/x"), &fork, Some("eve"));
        for (args, owner) in [
            ("pr view 5 -R Sam/r", "Sam"),
            ("pr view 5 --repo sam/r", "sam"),
            ("pr view 5 --repo=github.com/sam/r", "sam"),
            ("pr view 5 -Rsam/r", "sam"),
            ("pr view 5 -wRsam/r", "sam"),
            ("pr view https://github.com/sam/r/pull/5", "sam"),
            ("pr checkout --force https://github.com/sam/r/pull/5", "sam"),
            (
                "pr checkout -b https://github.com/eve/x https://github.com/sam/r/pull/5",
                "sam",
            ),
            ("pr view https://github.com/sam/r/pull/5 -R Sam/r", "Sam"),
            ("api repos/sam/r/pulls", "sam"),
            ("api -X POST /repos/sam/r/pulls -f body=repos/eve/x", "sam"),
            (
                "api -X POST -f title=x -F body=https://github.com/eve/x repos/sam/r/issues",
                "sam",
            ),
            ("api https://api.github.com/repos/sam/r/pulls?page=2", "sam"),
            ("pr view https://www.GitHub.com/sam/r/pull/5", "sam"),
            ("api --hostname api.github.com repos/sam/r", "sam"),
            ("api orgs/sam/members", "sam"),
            ("api --hostname github.com repos/sam/r", "sam"),
            ("repo view sam/r", "sam"),
            ("repo clone sam/r dir", "sam"),
            ("repo clone sam/r -- https://github.com/eve/x", "sam"),
            ("repo view --json name sam/r", "sam"),
            ("repo view -b main https://github.com/sam/r", "sam"),
            ("repo list sam", "sam"),
            ("repo list --limit 5 sam", "sam"),
            ("secret set X --org sam", "sam"),
            ("secret set X -o sam -b https://github.com/eve/x", "sam"),
            // A flag's value isn't a name, given either way.
            (
                "issue comment 5 -R sam/r --body https://github.com/eve/x",
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
            "pr view 5 -R ghe.example.com/sam/r",
            "pr view 5 -R sam",
            "pr view 5 -R",
            "api --hostname ghe.example.com repos/sam/r",
            "api --hostname=ghe.example.com repos/sam/r",
            "pr view https://sam.ghe.com/sam/r/pull/5",
            "pr view https://gitlab.com/eve/x",
            "api https://ghe.example.com/api/v3/repos/sam/r",
            "api --hostname github.com.example repos/sam/r",
            // gh can't say what it takes: neither can this.
            "nosuch 5",
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
        // Not a URL or a remote, so not a name: a branch, a number, `{owner}`,
        // a flag's value.
        for args in [
            "pr view feature/x",
            "api repos/{owner}/{repo}/pulls",
            "api graphql -f query=x",
            "pr view 5 --json https://github.com/eve/x",
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
