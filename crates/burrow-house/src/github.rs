//! What a check reads and posts through: one PR's repo on GitHub.

use std::cell::RefCell;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use burrow_github::{Api, call, call_repeating};
use serde_json::{Value, json};

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
    /// A path's bytes at a commit, as git keeps them: a file's content, a
    /// symlink's target, and nothing for a submodule, which has no blob.
    fn content(&self, path: &str, at: &str) -> Result<Vec<u8>, String>;
    /// Post a status on a commit.
    fn post(&self, sha: &str, status: &Status) -> Result<(), String>;
}

/// The repo's GitHub, through burrow-github (`gh api` and its token,
/// GH_TOKEN), which asks again when GitHub doesn't answer.
pub struct Real<A: Api> {
    pub repo: String,
    pub api: RefCell<A>,
}

impl<A: Api> Real<A> {
    pub fn new(repo: String, api: A) -> Self {
        Self {
            repo,
            api: RefCell::new(api),
        }
    }

    fn json(&self, path: &str) -> Result<Value, String> {
        Ok(call(&mut *self.api.borrow_mut(), "GET", path, None, None)?.json()?)
    }
}

/// A contents or blob answer's bytes, from the base64 it carries in lines.
fn decoded(what: &str, answer: &Value) -> Result<Vec<u8>, String> {
    let lines = answer["content"]
        .as_str()
        .ok_or_else(|| format!("{what}: GitHub gave no content"))?;
    let joined: String = lines.split_whitespace().collect();
    STANDARD
        .decode(joined)
        .map_err(|e| format!("{what}: its content isn't base64: {e}"))
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

impl<A: Api> GitHub for Real<A> {
    fn pull(&self, number: u64) -> Result<Value, String> {
        self.json(&format!("repos/{}/pulls/{number}", self.repo))
    }

    fn compare(&self, base: &str, head: &str) -> Result<Value, String> {
        self.json(&format!(
            "repos/{}/compare/{base}...{head}?per_page=1",
            self.repo
        ))
    }

    /// The contents API's answer carries a file of up to 1 MB; past that its
    /// encoding is `none`, and the file's blob, named by the same answer,
    /// carries it (up to 100 MB). A symlink to a file in the repo comes back
    /// as that file; one GitHub doesn't follow, as its target.
    fn content(&self, path: &str, at: &str) -> Result<Vec<u8>, String> {
        let url = format!("repos/{}/contents/{}?ref={at}", self.repo, encode(path));
        let entry = self.json(&url)?;
        match (entry["type"].as_str(), entry["encoding"].as_str()) {
            (Some("file"), Some("base64")) => decoded(&url, &entry),
            (Some("file"), Some("none")) => {
                let sha = entry["sha"]
                    .as_str()
                    .ok_or_else(|| format!("GET {url}: GitHub named no blob"))?;
                let blob = format!("repos/{}/git/blobs/{sha}", self.repo);
                decoded(&blob, &self.json(&blob)?)
            }
            (Some("symlink"), _) => entry["target"]
                .as_str()
                .map(|t| t.as_bytes().to_vec())
                .ok_or_else(|| format!("GET {url}: GitHub gave the link no target")),
            (Some("submodule"), _) => Ok(Vec::new()),
            _ => Err(format!("GET {url}: not a file, link or submodule")),
        }
    }

    fn post(&self, sha: &str, status: &Status) -> Result<(), String> {
        let url = format!("repos/{}/statuses/{sha}", self.repo);
        let body = json!({
            "state": status.state,
            "context": status.context,
            "description": status.description,
            "target_url": status.target_url,
        });
        call_repeating(&mut *self.api.borrow_mut(), "POST", &url, Some(&body))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use burrow_github::fake::{Fake, ok};

    fn real(gh: Fake) -> Real<Fake> {
        Real::new("sam/r".into(), gh)
    }

    // A file comes from the base64 GitHub sends in lines; one over 1 MB,
    // from its blob; a link GitHub doesn't follow, its target; a submodule,
    // nothing; a directory is refused.
    #[test]
    fn content() {
        let mut gh = Fake::default();
        let at = "repos/sam/r/contents/a%20b.rs?ref=4ead";
        gh.on(
            "GET",
            at,
            vec![
                ok(502, Value::Null, &[]),
                ok(
                    200,
                    json!({"type": "file", "encoding": "base64", "content": "eCB5\nCg==\n"}),
                    &[],
                ),
            ],
        );
        let r = real(gh);
        assert_eq!(r.content("a b.rs", "4ead").unwrap(), b"x y\n");
        assert_eq!(r.api.borrow().asked.len(), 2);

        let mut gh = Fake::default();
        gh.on(
            "GET",
            "repos/sam/r/contents/big",
            vec![ok(
                200,
                json!({"type": "file", "encoding": "none", "content": "", "sha": "b10b"}),
                &[],
            )],
        );
        gh.on(
            "GET",
            "repos/sam/r/git/blobs/b10b",
            vec![ok(
                200,
                json!({"encoding": "base64", "content": "AA=="}),
                &[],
            )],
        );
        assert_eq!(real(gh).content("big", "4ead").unwrap(), [0]);

        let mut gh = Fake::default();
        gh.on(
            "GET",
            "repos/sam/r/contents/dir",
            vec![ok(200, json!([]), &[])],
        );
        let e = real(gh).content("dir", "4ead").unwrap_err();
        assert!(e.ends_with("not a file, link or submodule"), "{e}");

        // A link GitHub doesn't follow is its target, a submodule nothing.
        let mut gh = Fake::default();
        gh.on(
            "GET",
            "repos/sam/r/contents/run",
            vec![ok(200, json!({"type": "symlink", "target": "../x"}), &[])],
        );
        gh.on(
            "GET",
            "repos/sam/r/contents/vendor/lib",
            vec![ok(200, json!({"type": "submodule", "sha": "5ab"}), &[])],
        );
        let r = real(gh);
        assert_eq!(r.content("run", "4ead").unwrap(), b"../x");
        assert!(r.content("vendor/lib", "4ead").unwrap().is_empty());
    }

    // A status that GitHub didn't answer for is posted again: the newest
    // for its context is the one that counts.
    #[test]
    fn post() {
        let mut gh = Fake::default();
        gh.on(
            "POST",
            "repos/sam/r/statuses/4ead",
            vec![ok(502, Value::Null, &[]), ok(201, json!({}), &[])],
        );
        let r = real(gh);
        let status = Status {
            context: "house/title (#7)".into(),
            state: "success",
            description: "ok".into(),
            target_url: "https://github.example/run".into(),
        };
        r.post("4ead", &status).unwrap();
        let api = r.api.borrow();
        assert_eq!(api.asked.len(), 2);
        assert_eq!(
            api.asked[1].2,
            Some(json!({
                "state": "success",
                "context": "house/title (#7)",
                "description": "ok",
                "target_url": "https://github.example/run",
            }))
        );
    }

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
