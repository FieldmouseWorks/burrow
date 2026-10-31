//! What a check reads and posts through: one PR's repo on GitHub.

use std::process::{Command, Stdio};

use serde_json::Value;

/// A commit status, as a check posts it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Status {
    pub context: String,
    /// `success` or `failure`.
    pub state: &'static str,
    pub description: String,
    pub target_url: String,
}

pub trait GitHub {
    /// The PR, as GitHub has it now.
    fn pull(&self, number: u64) -> Result<Value, String>;
    /// The comparison of two commits, `base...head`: their merge base and
    /// the files it changes, 300 at most, each with its patch when GitHub
    /// shows one.
    fn compare(&self, base: &str, head: &str) -> Result<Value, String>;
    /// A file's bytes at a commit.
    fn content(&self, path: &str, at: &str) -> Result<Vec<u8>, String>;
    /// Post a status on a commit.
    fn post(&self, sha: &str, status: &Status) -> Result<(), String>;
}

/// The repo's GitHub, through `gh api` and its token (GH_TOKEN).
pub struct Real {
    pub repo: String,
}

/// `gh api` with these arguments: what it printed, or why it failed.
fn gh(args: &[&str]) -> Result<Vec<u8>, String> {
    let out = Command::new("gh")
        .arg("api")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("gh: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

fn json(path: &str) -> Result<Value, String> {
    let out = gh(&[path]).map_err(|e| format!("GET {path}: {e}"))?;
    serde_json::from_slice(&out).map_err(|e| format!("GET {path}: {e}"))
}

/// A path as a URL's path: each part percent-encoded but for the characters
/// a URL never reserves, so a path comes through byte for byte.
pub fn encode(path: &str) -> String {
    let mut out = String::new();
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl GitHub for Real {
    fn pull(&self, number: u64) -> Result<Value, String> {
        json(&format!("repos/{}/pulls/{number}", self.repo))
    }

    fn compare(&self, base: &str, head: &str) -> Result<Value, String> {
        json(&format!(
            "repos/{}/compare/{base}...{head}?per_page=1",
            self.repo
        ))
    }

    fn content(&self, path: &str, at: &str) -> Result<Vec<u8>, String> {
        let url = format!("repos/{}/contents/{}?ref={at}", self.repo, encode(path));
        gh(&["-H", "Accept: application/vnd.github.raw", &url])
            .map_err(|e| format!("GET {url}: {e}"))
    }

    fn post(&self, sha: &str, status: &Status) -> Result<(), String> {
        let url = format!("repos/{}/statuses/{sha}", self.repo);
        gh(&[
            "-X",
            "POST",
            &url,
            "-f",
            &format!("state={}", status.state),
            "-f",
            &format!("context={}", status.context),
            "-f",
            &format!("description={}", status.description),
            "-f",
            &format!("target_url={}", status.target_url),
        ])
        .map(drop)
        .map_err(|e| format!("POST {url}: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A path is sent whole: a space, a percent sign, a backslash, a tab, a
    // newline, a question mark, anything past ASCII.
    #[test]
    fn paths() {
        assert_eq!(encode("src/main.rs"), "src/main.rs");
        assert_eq!(encode("my dir/run 100%"), "my%20dir/run%20100%25");
        assert_eq!(encode("bin/we\\ird"), "bin/we%5Cird");
        assert_eq!(encode("bin/tab\there"), "bin/tab%09here");
        assert_eq!(encode("bin/new\nline"), "bin/new%0Aline");
        assert_eq!(encode("a?ref=x#y"), "a%3Fref%3Dx%23y");
        assert_eq!(encode("é"), "%C3%A9");
    }
}
