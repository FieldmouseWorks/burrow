//! What a manifest follows: each `metadata.follow."URL"` table, the rev its
//! dependencies on that URL are pinned at, and what upstream changes it
//! calls inert.

use std::collections::BTreeMap;

use serde::Deserialize;

/// What in the upstream changes nothing the follower builds from it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Rules {
    /// Paths: one ending in `/` is a directory, everything under it; any
    /// other, that one file.
    #[serde(default)]
    pub inert: Vec<String>,
    /// A file, and the lines in it (each as its text starts, past any
    /// indent) that it differs in without counting: a file whose other
    /// lines are the same at both commits is inert.
    #[serde(default)]
    pub inert_lines: BTreeMap<String, Vec<String>>,
}

/// One upstream a manifest follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Follow {
    /// The dependencies' `git`, as the manifest spells it.
    pub url: String,
    /// Its repo on GitHub, OWNER/NAME.
    pub upstream: String,
    pub rules: Rules,
    /// The rev every dependency on the URL is pinned at.
    pub pin: String,
    /// Those dependencies' names, in the manifest's order of tables.
    pub deps: Vec<String>,
}

/// OWNER/NAME of a GitHub URL, with or without `.git`.
pub fn github_repo(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://github.com/")?
        .trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return None;
    };
    (!owner.is_empty() && !name.is_empty()).then(|| format!("{owner}/{name}"))
}

fn same_url(a: &str, b: &str) -> bool {
    let norm = |u: &str| {
        let u = u.trim_end_matches('/');
        u.strip_suffix(".git").unwrap_or(u).to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// A rev the pass can put a head in place of: a whole commit id.
pub fn is_commit(rev: &str) -> bool {
    rev.len() == 40 && rev.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Every dependency table, anywhere in the manifest but its metadata, whose
/// `git` is the URL and that has a `rev`: its rev and its name.
fn pins(table: &toml::Table, url: &str, found: &mut BTreeMap<String, Vec<String>>) {
    for (key, value) in table {
        let Some(t) = value.as_table() else { continue };
        if key == "metadata" {
            continue;
        }
        let git = t.get("git").and_then(toml::Value::as_str);
        let rev = t.get("rev").and_then(toml::Value::as_str);
        match (git, rev) {
            (Some(git), Some(rev)) if same_url(git, url) => {
                found.entry(rev.to_string()).or_default().push(key.clone());
            }
            _ => pins(t, url, found),
        }
    }
}

/// What the manifest follows: none when it has no `metadata.follow`.
pub fn follows(text: &str) -> Result<Vec<Follow>, String> {
    let doc: toml::Table = toml::from_str(text).map_err(|e| format!("isn't TOML: {e}"))?;
    let meta = ["workspace", "package"].iter().find_map(|top| {
        doc.get(*top)
            .and_then(|t| t.get("metadata"))
            .and_then(|m| m.get("follow"))
    });
    let Some(meta) = meta else {
        return Ok(Vec::new());
    };
    let meta = meta
        .as_table()
        .ok_or("metadata.follow isn't a table of URLs")?;
    let mut out = Vec::new();
    for (url, rules) in meta {
        let at = format!("metadata.follow.\"{url}\"");
        let rules: Rules = rules.clone().try_into().map_err(|e| format!("{at}: {e}"))?;
        let upstream = github_repo(url)
            .ok_or_else(|| format!("{at}: not a repo on GitHub (https://github.com/OWNER/NAME)"))?;
        let mut found = BTreeMap::new();
        pins(&doc, url, &mut found);
        let n = found.len();
        let mut found = found.into_iter();
        let (Some((pin, deps)), None) = (found.next(), found.next()) else {
            return Err(if n == 0 {
                format!("{at}: no dependency takes {url} at a rev")
            } else {
                format!(
                    "{at}: its dependencies take {url} at more than one rev: pin them all at one"
                )
            });
        };
        if !is_commit(&pin) {
            return Err(format!("{at}: {pin} isn't a whole commit id"));
        }
        out.push(Follow {
            url: url.clone(),
            upstream,
            rules,
            pin,
            deps,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "cdde15ec2e4d4a03017e00b25e617d2d33a014fa";
    const B: &str = "a570d690605379fc78ed72a1ed716478286785db";

    fn manifest(core: &str, github: &str) -> String {
        format!(
            r#"[workspace]
members = ["a"]

[workspace.dependencies]
serde = "1"
up-core = {{ git = "https://github.com/sam/Up", rev = "{core}" }}
up-github = {{ git = "https://github.com/sam/up.git", rev = "{github}" }}
other = {{ git = "https://github.com/sam/other", rev = "{core}" }}

[workspace.metadata.follow."https://github.com/sam/Up"]
inert = [".github/", "flake.lock"]
inert-lines = {{ "flake.nix" = ["inputs.home.url ="] }}
"#
        )
    }

    #[test]
    fn reads_a_follow_and_its_pin() {
        let got = follows(&manifest(A, A)).unwrap();
        assert_eq!(
            got,
            vec![Follow {
                url: "https://github.com/sam/Up".into(),
                upstream: "sam/Up".into(),
                rules: Rules {
                    inert: vec![".github/".into(), "flake.lock".into()],
                    inert_lines: [("flake.nix".into(), vec!["inputs.home.url =".into()])].into(),
                },
                pin: A.into(),
                // The same repo, however the URL is spelled; not another.
                deps: vec!["up-core".into(), "up-github".into()],
            }]
        );
    }

    #[test]
    fn refuses_what_it_cant_move_as_one() {
        let e = follows(&manifest(A, B)).unwrap_err();
        assert!(e.contains("more than one rev"), "{e}");
        let e = follows(&manifest("v1", "v1")).unwrap_err();
        assert!(e.contains("v1 isn't a whole commit id"), "{e}");
        let none = manifest(A, A).replace("rev = ", "branch = ");
        let e = follows(&none).unwrap_err();
        assert!(
            e.contains("no dependency takes https://github.com/sam/Up at a rev"),
            "{e}"
        );
        let elsewhere = manifest(A, A).replace(
            "follow.\"https://github.com/sam/Up\"",
            "follow.\"https://example.com/sam/Up\"",
        );
        let e = follows(&elsewhere).unwrap_err();
        assert!(e.contains("not a repo on GitHub"), "{e}");
        let unknown = manifest(A, A).replace("inert-lines", "inert_lines");
        let e = follows(&unknown).unwrap_err();
        assert!(e.contains("unknown field `inert_lines`"), "{e}");
    }

    #[test]
    fn follows_nothing_without_its_table() {
        assert_eq!(follows("[package]\nname = \"a\"\n").unwrap(), vec![]);
        let package = "[package]\nname = \"a\"\n[dependencies]\nup = { git = \"https://github.com/sam/up\", rev = \"".to_string()
            + A
            + "\" }\n[package.metadata.follow.\"https://github.com/sam/up\"]\n";
        let got = follows(&package).unwrap();
        assert_eq!(
            (got[0].deps.clone(), got[0].rules.clone()),
            (vec!["up".to_string()], Rules::default())
        );
    }
}
