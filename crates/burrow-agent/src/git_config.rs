//! A clone's git config as a plugin may read it. A host repository's can
//! hold a token (an `http.extraheader`, a remote URL's password, a helper),
//! so only what git needs to read the repository and name its branches and
//! remotes stays.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use crate::Agent;

/// Rewrites every config under `git`: its own, its worktrees', its
/// submodules'.
pub fn sanitize(agent: &Agent, git: &Path) -> Result<(), String> {
    let mut configs = Vec::new();
    find(git, &mut configs)?;
    for config in configs {
        let kept: Vec<(String, String)> = read(agent, &config)?
            .into_iter()
            .filter_map(|(key, value)| keep(&key, &value).map(|value| (key, value)))
            .collect();
        let io = |e: std::io::Error| format!("{}: {e}", config.display());
        fs::remove_file(&config).map_err(io)?;
        File::create_new(&config).map_err(io)?;
        for (key, value) in kept {
            let status = git_command(agent)
                .arg("--file")
                .arg(&config)
                .args(["--add", &key, &value])
                .status()
                .map_err(io)?;
            if !status.success() {
                return Err(format!("{}: {key} wouldn't write", config.display()));
            }
        }
    }
    Ok(())
}

/// git's directories under `dir` that aren't objects, refs or logs, where
/// a branch can be called `config`.
fn find(dir: &Path, into: &mut Vec<PathBuf>) -> Result<(), String> {
    let io = |e: std::io::Error| format!("{}: {e}", dir.display());
    for entry in fs::read_dir(dir).map_err(io)? {
        let entry = entry.map_err(io)?;
        let (name, kind) = (entry.file_name(), entry.file_type().map_err(io)?);
        let is = |names: &[&str]| names.iter().any(|n| name == *n);
        if kind.is_dir() {
            if !is(&["objects", "refs", "logs"]) {
                find(&entry.path(), into)?;
            }
        } else if is(&["config", "config.worktree"]) {
            into.push(entry.path());
        }
    }
    Ok(())
}

fn git_command(agent: &Agent) -> std::process::Command {
    let mut c = agent.command("git");
    c.env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir("/")
        .stdin(Stdio::null())
        .arg("config");
    c
}

fn read(agent: &Agent, config: &Path) -> Result<Vec<(String, String)>, String> {
    let listed = git_command(agent)
        .arg("--file")
        .arg(config)
        .args(["--no-includes", "--list", "-z"])
        .output()
        .map_err(|e| format!("git: {e}"))?;
    if !listed.status.success() {
        return Err(format!("{}: git can't read it", config.display()));
    }
    Ok(String::from_utf8_lossy(&listed.stdout)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        // A key with no `=` is true.
        .map(|entry| entry.split_once('\n').unwrap_or((entry, "true")))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect())
}

/// The value to keep for `key`, if any.
fn keep(key: &str, value: &str) -> Option<String> {
    let (section, rest) = key.split_once('.')?;
    let last = rest.rsplit('.').next()?;
    // A URL can be the name between them, login and all.
    if rest.contains("://") {
        return None;
    }
    match (section, last) {
        ("core", "repositoryformatversion" | "bare" | "filemode" | "worktree")
        | ("branch", "merge")
        | (
            "extensions",
            "objectformat" | "refstorage" | "worktreeconfig" | "relativeworktrees" | "partialclone",
        )
        | ("remote", "fetch") => Some(value.to_owned()),
        // Either can be a URL in a name's place.
        ("branch", "remote") | ("remote" | "submodule", "url") => Some(without_login(value)),
        _ => None,
    }
}

/// `url` with no user, password or query.
fn without_login(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        // scp's user@host:path: the user goes too, in case it's a token.
        return match url.split_once(':') {
            Some((host, path)) if !host.contains('/') => {
                let host = host.rsplit('@').next().unwrap_or(host);
                match path.rsplit_once('@') {
                    Some((_, after)) => format!("{host}:{after}"),
                    None => format!("{host}:{path}"),
                }
            }
            _ => url.to_owned(),
        };
    };
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let host = rest.rsplit('@').next().unwrap_or(rest);
    format!("{scheme}://{host}")
}

#[cfg(test)]
mod tests;
