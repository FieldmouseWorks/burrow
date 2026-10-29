//! A gh call read the way gh reads it, with gh's own help (`gh help WORDS`)
//! for what only gh knows: which words are subcommands (aliases too), which
//! flags take a value, and what each positional argument is.
//!
//! The command is found as cobra finds it. At each level, the first word
//! that isn't a flag or a flag's value is the next subcommand if gh's help
//! says it is one, and a flag the level doesn't know is taken to have a
//! value. The rest is parsed as pflag parses it, with the command's flags and
//! the ones it inherits: `--name value`, `--name=value`, `-n value`,
//! `-nvalue`, `-n=value`, short flags run together, and `--` ending the
//! flags. A flag gh's help doesn't list is refused, as gh refuses it.

use std::collections::HashMap;

use crate::error::Error;

/// A gh call, read.
pub struct Call {
    /// The command, aliases resolved: `pr checkout` for `co`.
    pub path: Vec<String>,
    /// The values given to its flags that take one, by long name.
    pub values: Vec<(String, String)>,
    /// Its positional arguments, each with the names its usage gives that
    /// place (`url` in `[<number> | <url> | <branch>]`), or none.
    pub args: Vec<(String, Vec<String>)>,
}

pub fn read(
    args: &[String],
    help: &dyn Fn(&[String]) -> Result<String, Error>,
) -> Result<Call, Error> {
    let mut command = Help::of(&help(&[])?, &[])?;
    let mut rest = args.to_vec();
    while command.parent {
        let Some(&at) = command.words(&rest).first() else {
            break;
        };
        let mut asked = command.path.clone();
        asked.push(rest[at].clone());
        let next = Help::of(&help(&asked)?, &asked)?;
        // gh's help for a word that isn't a subcommand is its parent's.
        if next.path.len() <= command.path.len() {
            break;
        }
        rest.remove(at);
        command = next;
    }
    command.parse(&rest)
}

/// What gh's help says of one command.
#[derive(Debug)]
struct Help {
    path: Vec<String>,
    /// Its usage's places for positional arguments, in order.
    slots: Vec<Slot>,
    /// Its flags and the ones it inherits, by long name: whether each takes
    /// a value.
    long: HashMap<String, bool>,
    /// Their short names.
    short: HashMap<char, String>,
    /// Whether it has subcommands.
    parent: bool,
}

#[derive(Debug, PartialEq)]
struct Slot {
    names: Vec<String>,
    /// `<files>...`: every argument from here on.
    variadic: bool,
}

impl Help {
    /// `asked` is what gh was asked about, for saying so when it knows
    /// nothing of it.
    fn of(help: &str, asked: &[String]) -> Result<Self, Error> {
        let unknown = || {
            let call: Vec<&str> = std::iter::once("gh")
                .chain(asked.iter().map(String::as_str))
                .collect();
            Error::refused(format!("gh's help has no usage for `{}`", call.join(" ")))
        };
        let mut lines = help.lines();
        lines.find(|l| *l == "USAGE").ok_or_else(unknown)?;
        let usage = lines
            .next()
            .map(str::trim)
            .and_then(|u| u.strip_prefix("gh ").or((u == "gh").then_some("")))
            .ok_or_else(unknown)?;
        let mut path = Vec::new();
        let mut rest = usage.trim_start();
        while let Some(word) = rest.split_whitespace().next() {
            if word.starts_with(['<', '[', '{', '-']) {
                break;
            }
            path.push(word.to_owned());
            rest = rest[word.len()..].trim_start();
        }
        let mut found = Self {
            path,
            slots: slots(rest),
            long: HashMap::new(),
            short: HashMap::new(),
            parent: false,
        };
        found.read_sections(help);
        Ok(found)
    }

    /// Its flags, from the FLAGS sections:
    /// `  -b, --body text   The comment body text` takes a value, and
    /// `  -w, --web         Open the web browser` doesn't. A section's heading
    /// is a line that isn't indented.
    fn read_sections(&mut self, help: &str) {
        let mut in_flags = false;
        for line in help.lines().filter(|l| !l.is_empty()) {
            if !line.starts_with(' ') {
                in_flags = line.ends_with("FLAGS");
                self.parent |= line.ends_with("COMMANDS");
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
            let Some(long) = long.strip_prefix("--") else {
                continue;
            };
            let (name, takes) = match long.split_once(' ') {
                Some((name, _type)) => (name, true),
                None => (long, false),
            };
            if let Some(c) = short {
                self.short.insert(c, name.to_owned());
            }
            self.long.insert(name.to_owned(), takes);
        }
    }

    /// Whether `s` takes the argument after it, as cobra decides that while
    /// it looks for subcommands: unless this level knows the flag doesn't.
    fn takes_next(&self, s: &str) -> bool {
        if let Some(name) = s.strip_prefix("--") {
            !name.contains('=') && self.long.get(name) != Some(&false)
        } else if let Some(short) = s.strip_prefix('-').filter(|s| s.len() == 1) {
            let c = short.chars().next().unwrap_or_default();
            self.short.get(&c).is_none_or(|name| self.long[name])
        } else {
            false
        }
    }

    /// Where the words that aren't flags or their values are, up to `--`.
    fn words(&self, args: &[String]) -> Vec<usize> {
        let mut words = Vec::new();
        let mut i = 0;
        while i < args.len() {
            let s = &args[i];
            if s == "--" {
                break;
            }
            if self.takes_next(s) {
                i += 1;
            } else if !s.is_empty() && !s.starts_with('-') {
                words.push(i);
            }
            i += 1;
        }
        words
    }

    /// `args`, as this command's flags and positional arguments.
    fn parse(self, args: &[String]) -> Result<Call, Error> {
        let command = format!("`gh {}`", self.path.join(" "));
        let wants = |name: &str| Error::refused(format!("{command}: --{name} wants a value"));
        let mut values = Vec::new();
        let mut positional = Vec::new();
        let mut it = args.iter();
        while let Some(s) = it.next() {
            if s == "--" {
                positional.extend(it.by_ref().cloned());
            } else if let Some(long) = s.strip_prefix("--") {
                let (name, attached) = match long.split_once('=') {
                    Some((n, v)) => (n, Some(v.to_owned())),
                    None => (long, None),
                };
                match self.long.get(name) {
                    None => {
                        return Err(Error::refused(format!(
                            "{command} has no flag --{name}, by gh's help"
                        )));
                    }
                    // A flag that doesn't take a value, given one or not.
                    Some(false) => {}
                    Some(true) => {
                        let value = attached.or_else(|| it.next().cloned());
                        values.push((name.to_owned(), value.ok_or_else(|| wants(name))?));
                    }
                }
            } else if let Some(mut shorts) = s.strip_prefix('-').filter(|s| !s.is_empty()) {
                while let Some(c) = shorts.chars().next() {
                    let Some(name) = self.short.get(&c) else {
                        return Err(Error::refused(format!(
                            "{command} has no flag -{c}, by gh's help"
                        )));
                    };
                    let after = &shorts[c.len_utf8()..];
                    let takes = self.long[name];
                    // -n=value, for either kind.
                    if let Some(value) = after.strip_prefix('=').filter(|v| !v.is_empty()) {
                        if takes {
                            values.push((name.clone(), value.to_owned()));
                        }
                        break;
                    }
                    if !takes {
                        shorts = after;
                        continue;
                    }
                    let value = if after.is_empty() {
                        it.next().cloned().ok_or_else(|| wants(name))?
                    } else {
                        after.to_owned()
                    };
                    values.push((name.clone(), value));
                    break;
                }
            } else {
                positional.push(s.clone());
            }
        }
        let args = positional
            .into_iter()
            .enumerate()
            .map(|(i, a)| {
                let slot = self
                    .slots
                    .get(i)
                    .or_else(|| self.slots.last().filter(|s| s.variadic));
                (a, slot.map(|s| s.names.clone()).unwrap_or_default())
            })
            .collect();
        Ok(Call {
            path: self.path,
            values,
            args,
        })
    }
}

/// The places for positional arguments in a usage, after the command:
/// `{<number> | <url>} <destination-repo>` is two. A group that's about
/// flags (`[flags]`, `[--owner | --repo]`, `[-- <gitflags>...]`) is none.
fn slots(usage: &str) -> Vec<Slot> {
    let mut slots = Vec::new();
    let mut rest = usage.trim_start();
    while let Some(first) = rest.chars().next() {
        let end = match first {
            '[' | '{' => {
                let mut depth = 0;
                rest.char_indices()
                    .find(|&(_, c)| {
                        depth += match c {
                            '[' | '{' => 1,
                            ']' | '}' => -1,
                            _ => 0,
                        };
                        depth == 0
                    })
                    .map_or(rest.len(), |(i, c)| i + c.len_utf8())
            }
            '<' => rest.find('>').map_or(rest.len(), |i| i + 1),
            _ => rest.find(char::is_whitespace).unwrap_or(rest.len()),
        };
        let (group, after) = rest.split_at(end);
        let variadic = after.starts_with("...") || group.ends_with("...]");
        rest = after.trim_start_matches('.').trim_start();
        let inner = match first {
            '[' | '{' => group[1..].strip_suffix([']', '}']).unwrap_or(&group[1..]),
            '<' => group,
            _ => continue,
        };
        if inner.trim_start().starts_with('-') {
            continue;
        }
        let names: Vec<String> = inner
            .split('<')
            .skip(1)
            .filter_map(|p| p.split_once('>').map(|(name, _)| name.to_owned()))
            .collect();
        if !names.is_empty() {
            slots.push(Slot { names, variadic });
        }
    }
    slots
}

#[cfg(test)]
pub mod fake {
    //! gh's help, in gh 2.103.2's layout, for the commands the tests call.

    use super::*;

    /// Path, aliases under the same parent, usage after the path, FLAGS lines.
    const COMMANDS: &[(&str, &[&str], &str, &str)] = &[
        (
            "",
            &[],
            "<command> <subcommand> [flags]",
            "  --help      Show help for command\n  --version   Show gh version\n",
        ),
        (
            "pr",
            &[],
            "<command> [flags]",
            "  -R, --repo [HOST/]OWNER/REPO   Select another repository using the [HOST/]OWNER/REPO format\n",
        ),
        (
            "pr view",
            &[],
            "[<number> | <url> | <branch>] [flags]",
            "  -c, --comments          View pull request comments\n  -q, --jq expression     Filter JSON output using a jq expression\n      --json fields       Output JSON with the specified fields\n  -w, --web               Open a pull request in the browser\n",
        ),
        (
            "pr checkout",
            &["co"],
            "[<number> | <url> | <branch>] [flags]",
            "  -b, --branch string        Local branch name to use (default [the name of the head branch])\n  -f, --force                Reset the existing local branch to the latest state of the pull request\n",
        ),
        (
            "issue",
            &[],
            "<command> [flags]",
            "  -R, --repo [HOST/]OWNER/REPO   Select another repository using the [HOST/]OWNER/REPO format\n",
        ),
        (
            "issue comment",
            &[],
            "{<number> | <url>} [flags]",
            "  -b, --body text        The comment body text\n  -F, --body-file file   Read body text from file (use \"-\" to read from standard input)\n  -e, --editor           Skip prompts and open the text editor to write the body in\n  -w, --web              Open the web browser to write the comment\n",
        ),
        (
            "issue transfer",
            &[],
            "{<number> | <url>} <destination-repo>",
            "",
        ),
        ("issue artifact", &[], "<command>", ""),
        (
            "issue artifact create",
            &[],
            "{<issue-number> | <issue-url>} [<file>[#<name>] | <url>]... [flags]",
            "",
        ),
        ("repo", &[], "<command>", ""),
        (
            "repo list",
            &["ls"],
            "[<owner>] [flags]",
            "  -q, --jq expression       Filter JSON output using a jq expression\n  -L, --limit int           Maximum number of repositories to list (default 30)\n      --source              Show only non-forks\n",
        ),
        (
            "repo clone",
            &[],
            "<repository> [<directory>] [-- <gitflags>...]",
            "",
        ),
        (
            "repo view",
            &[],
            "[<repository>] [flags]",
            "  -b, --branch string     View a specific branch of the repository\n      --json fields       Output JSON with the specified fields\n  -w, --web               Open a repository in the browser\n",
        ),
        (
            "repo read-file",
            &[],
            "<path> [flags]",
            "      --ref string   The branch, tag, or commit to read from\n",
        ),
        (
            "api",
            &[],
            "<endpoint> [flags]",
            "  -F, --field key=value          Add a typed parameter in key=value format\n      --hostname string          The GitHub hostname for the request (default \"github.com\")\n  -i, --include                  Include HTTP response status line and headers in the output\n  -X, --method string            The HTTP method for the request (default \"GET\")\n      --paginate                 Make additional HTTP requests to fetch all pages of results\n  -f, --raw-field key=value      Add a string parameter in key=value format\n",
        ),
        (
            "secret",
            &[],
            "<command> [flags]",
            "  -R, --repo [HOST/]OWNER/REPO   Select another repository using the [HOST/]OWNER/REPO format\n",
        ),
        (
            "secret set",
            &[],
            "<secret-name> [flags]",
            "  -b, --body string          The value for the secret (reads from standard input if not specified)\n  -o, --org organization     Set organization secret\n  -u, --user                 Set a secret for your user\n",
        ),
        ("search", &[], "<command>", ""),
        (
            "search issues",
            &[],
            "[<query>] [flags]",
            "      --owner strings   Filter on repository owner\n  -R, --repo strings    Filter on repository\n",
        ),
    ];

    /// gh's own alias, at the top: `gh co`.
    const ALIASES: &[(&str, &str)] = &[("co", "pr checkout")];

    /// `gh help WORDS`: the command cobra finds for them, or nothing for a
    /// first word that isn't one, as gh prints it.
    pub fn help(words: &[String]) -> Result<String, Error> {
        let mut at = "";
        for w in words {
            let child = COMMANDS
                .iter()
                .find(|(path, aliases, ..)| {
                    let (parent, name) = path.rsplit_once(' ').unwrap_or(("", path));
                    !path.is_empty() && parent == at && (name == w || aliases.contains(&w.as_str()))
                })
                .map(|c| c.0)
                .or_else(|| {
                    let alias = ALIASES.iter().find(|(a, _)| at.is_empty() && a == w);
                    alias.map(|(_, path)| *path)
                });
            match child {
                Some(path) => at = path,
                None if at.is_empty() => return Ok(String::new()),
                None => break,
            }
        }
        let (path, _, usage, flags) = COMMANDS.iter().find(|c| c.0 == at).unwrap();
        let children: Vec<_> = COMMANDS
            .iter()
            .filter(|c| !c.0.is_empty() && c.0.rsplit_once(' ').map_or("", |p| p.0) == at)
            .collect();
        let mut out = format!(
            "Does things.\n\nUSAGE\n  gh {}{}{usage}\n\n",
            path,
            if path.is_empty() { "" } else { " " }
        );
        if !children.is_empty() {
            out += "AVAILABLE COMMANDS\n";
            for c in children {
                out += &format!("  {}:  Does a thing\n", c.0.rsplit(' ').next().unwrap());
            }
            out += "\n";
        }
        if !flags.is_empty() {
            out += &format!("FLAGS\n{flags}\n");
        }
        if !path.is_empty() {
            out += "INHERITED FLAGS\n      --help   Show help for command\n";
            let mut p = *path;
            while let Some((parent, _)) = p.rsplit_once(' ') {
                out += COMMANDS.iter().find(|c| c.0 == parent).unwrap().3;
                p = parent;
            }
            out += "\n";
        }
        out += &format!(
            "EXAMPLES\n  -b, --body text   not a flag\n  $ gh {path} --web\n\nLEARN MORE\n  Use `gh <command> <subcommand> --help` for more information about a command.\n"
        );
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::fake::help;
    use super::*;

    fn read(s: &str) -> Result<Call, Error> {
        let args: Vec<String> = s
            .split(' ')
            .filter(|a| !a.is_empty())
            .map(str::to_owned)
            .collect();
        super::read(&args, &help)
    }

    fn values(s: &str) -> Vec<(String, String)> {
        read(s).unwrap().values
    }

    #[test]
    fn reads_which_flags_take_a_value_from_the_help() {
        let comment = Help::of(&help(&["issue".into(), "comment".into()]).unwrap(), &[]).unwrap();
        assert_eq!(comment.long.get("body"), Some(&true));
        assert_eq!(comment.long.get("web"), Some(&false));
        assert_eq!(comment.long.get("repo"), Some(&true));
        assert_eq!(comment.long.get("help"), Some(&false));
        assert_eq!(comment.short.get(&'b').map(String::as_str), Some("body"));
        assert_eq!(comment.short.get(&'R').map(String::as_str), Some("repo"));
        assert!(!comment.parent);
        assert!(
            Help::of(&help(&["issue".into()]).unwrap(), &[])
                .unwrap()
                .parent
        );
        // A blank line in a section doesn't end it; an example isn't a flag.
        let h = Help::of("USAGE\n  gh x\n\nFLAGS\n  -a, --aa x   y\n\n  -b, --bb x   y\n\nEXAMPLES\n  -c, --cc x   y\n", &[]).unwrap();
        assert_eq!(h.long.len(), 2);
        assert!(h.long.contains_key("aa") && h.long.contains_key("bb"));
        // gh printed nothing, or no usage: refused.
        assert!(Help::of("", &[]).is_err());
        assert!(Help::of("Some topic.\n", &[]).is_err());
    }

    #[test]
    fn finds_the_command_as_cobra_does() {
        for (args, path) in [
            ("pr view 5", "pr view"),
            ("pr view feature/x -R sam/r", "pr view"),
            // Flags before and between the subcommands.
            ("issue -R sam/r comment 5", "issue comment"),
            ("--repo sam/r issue comment 5", "issue comment"),
            ("-R sam/r issue comment 5", "issue comment"),
            ("pr -R=sam/r view", "pr view"),
            // Aliases, as gh's help names what they're for.
            ("co 5", "pr checkout"),
            ("repo ls sam", "repo list"),
            // A word that isn't a subcommand ends the search.
            ("pr nosuch 5", "pr"),
            ("api repos/sam/r/pulls", "api"),
            ("--version", ""),
        ] {
            assert_eq!(read(args).unwrap().path.join(" "), path, "{args}");
        }
        // What isn't a subcommand stays the call's.
        assert_eq!(read("pr nosuch 5").unwrap().args.len(), 2);
        // gh knows no such command.
        assert!(read("nosuch 5").is_err());
    }

    #[test]
    fn parses_flags_as_pflag_does() {
        let v = |n: &str, x: &str| (n.to_owned(), x.to_owned());
        assert_eq!(values("pr view 5 -R sam/r"), [v("repo", "sam/r")]);
        assert_eq!(values("pr view 5 --repo=sam/r"), [v("repo", "sam/r")]);
        assert_eq!(values("pr view 5 -Rsam/r"), [v("repo", "sam/r")]);
        assert_eq!(values("pr view 5 -R=sam/r"), [v("repo", "sam/r")]);
        assert_eq!(values("pr view 5 -wcRsam/r"), [v("repo", "sam/r")]);
        assert_eq!(values("pr view 5 -w=true"), []);
        assert_eq!(
            values("issue -R sam/r comment 5 --body https://github.com/eve/x"),
            [v("repo", "sam/r"), v("body", "https://github.com/eve/x")]
        );
        assert_eq!(
            read("issue comment 5 -b -R").unwrap().args,
            [("5".to_owned(), vec!["number".to_owned(), "url".to_owned()])]
        );
        // A flag gh doesn't list, or one missing its value: refused.
        for args in [
            "pr view 5 --nosuch",
            "pr view 5 -z",
            "pr view 5 -R",
            "pr view 5 --repo",
            "pr view -wR",
        ] {
            assert!(read(args).is_err(), "{args}");
        }
    }

    #[test]
    fn names_each_argument_by_its_place_in_the_usage() {
        let args = |s: &str| -> Vec<(String, String)> {
            read(s)
                .unwrap()
                .args
                .into_iter()
                .map(|(a, names)| (a, names.join("|")))
                .collect()
        };
        let a = |x: &str, n: &str| (x.to_owned(), n.to_owned());
        assert_eq!(
            args("issue transfer 5 sam/r"),
            [a("5", "number|url"), a("sam/r", "destination-repo")]
        );
        // After --, still the command's own; past the usage's places, none.
        assert_eq!(args("api -- repos/sam/r"), [a("repos/sam/r", "endpoint")]);
        assert_eq!(
            args("repo clone sam/r dir -- --depth 1"),
            [
                a("sam/r", "repository"),
                a("dir", "directory"),
                a("--depth", ""),
                a("1", "")
            ]
        );
        assert_eq!(
            args("search issues -- https://github.com/eve/x"),
            [a("https://github.com/eve/x", "query")]
        );
        assert_eq!(args("repo read-file docs/x.md"), [a("docs/x.md", "path")]);
    }

    #[test]
    fn reads_a_usage_s_places() {
        let slot = |names: &[&str], variadic| Slot {
            names: names.iter().map(|n| n.to_string()).collect(),
            variadic,
        };
        assert_eq!(
            slots("{<number> | <url>} <destination-repo>"),
            [
                slot(&["number", "url"], false),
                slot(&["destination-repo"], false)
            ]
        );
        assert_eq!(
            slots("<repository> [<directory>] [-- <gitflags>...]"),
            [slot(&["repository"], false), slot(&["directory"], false)]
        );
        assert_eq!(
            slots("[<file-path> | oci://<image-uri>] [--owner | --repo] [flags]"),
            [slot(&["file-path", "image-uri"], false)]
        );
        assert_eq!(
            slots("{<issue-number> | <issue-url>} [<file>[#<name>] | <url>]... [flags]"),
            [
                slot(&["issue-number", "issue-url"], false),
                slot(&["file", "name", "url"], true)
            ]
        );
        assert_eq!(
            slots("<tag> <files>..."),
            [slot(&["tag"], false), slot(&["files"], true)]
        );
        assert_eq!(
            slots("[<task description>] [flags]"),
            [slot(&["task description"], false)]
        );
        assert_eq!(
            slots("<repository> [<skill[@version]>] [flags]"),
            [
                slot(&["repository"], false),
                slot(&["skill[@version]"], false)
            ]
        );
        assert!(slots("[flags]").is_empty() && slots("").is_empty());
    }
}
