//! What a manifest follows: each `metadata.follow."URL"` table, the rev its
//! dependencies on that URL are pinned at, and what upstream changes it
//! calls inert.

use std::collections::BTreeMap;
use std::ops::Range;

use serde::Deserialize;
use toml::Spanned;
use toml::de::{DeTable, DeValue};

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
    /// Those dependencies' names.
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

/// A dependency on the URL at a rev: its name, and where its `rev` value
/// sits in the manifest's text.
struct Pin {
    dep: String,
    at: Range<usize>,
}

/// Every dependency table, anywhere in the manifest but its metadata, whose
/// `git` is the URL and that has a `rev`, by that rev. The one walk both
/// what a manifest follows and the move of its pin read, so a move touches
/// exactly the tables a follow names.
fn pins(table: &DeTable, url: &str, found: &mut BTreeMap<String, Vec<Pin>>) {
    for (key, value) in table {
        let DeValue::Table(t) = value.get_ref() else {
            continue;
        };
        if key.get_ref() == "metadata" {
            continue;
        }
        let git = t.get("git").and_then(|v| v.get_ref().as_str());
        let rev = t
            .get("rev")
            .and_then(|v| v.get_ref().as_str().map(|r| (r, v.span())));
        match (git, rev) {
            (Some(git), Some((rev, at))) if same_url(git, url) => {
                found.entry(rev.to_string()).or_default().push(Pin {
                    dep: key.get_ref().to_string(),
                    at,
                });
            }
            _ => pins(t, url, found),
        }
    }
}

/// The URL's one rev in the manifest, and its dependencies on it.
fn pinned(doc: &DeTable, url: &str, at: &str) -> Result<(String, Vec<Pin>), String> {
    let mut found = BTreeMap::new();
    pins(doc, url, &mut found);
    let n = found.len();
    let mut found = found.into_iter();
    let (Some(one), None) = (found.next(), found.next()) else {
        return Err(if n == 0 {
            format!("{at}: no dependency takes {url} at a rev")
        } else {
            format!("{at}: its dependencies take {url} at more than one rev: pin them all at one")
        });
    };
    Ok(one)
}

fn parse(text: &str) -> Result<DeTable<'_>, String> {
    DeTable::parse(text)
        .map(Spanned::into_inner)
        .map_err(|e| format!("isn't TOML: {e}"))
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
    let spans = parse(text)?;
    let mut out = Vec::new();
    for (url, rules) in meta {
        let at = format!("metadata.follow.\"{url}\"");
        let rules: Rules = rules.clone().try_into().map_err(|e| format!("{at}: {e}"))?;
        let upstream = github_repo(url)
            .ok_or_else(|| format!("{at}: not a repo on GitHub (https://github.com/OWNER/NAME)"))?;
        let (pin, deps) = pinned(&spans, url, &at)?;
        if !is_commit(&pin) {
            return Err(format!("{at}: {pin} isn't a whole commit id"));
        }
        out.push(Follow {
            url: url.clone(),
            upstream,
            rules,
            pin,
            deps: deps.into_iter().map(|p| p.dep).collect(),
        });
    }
    Ok(out)
}

/// The manifest with the follow's dependencies, and only theirs, moved from
/// its pin to `head`: every other byte as it was, another dependency at the
/// same rev included.
pub fn moved(text: &str, follow: &Follow, head: &str) -> Result<String, String> {
    let at = format!("metadata.follow.\"{}\"", follow.url);
    let (pin, deps) = pinned(&parse(text)?, &follow.url, &at)?;
    if pin != follow.pin {
        return Err(format!("{at}: pinned at {pin} here, not {}", follow.pin));
    }
    let mut out = text.to_string();
    let mut deps = deps;
    deps.sort_by_key(|p| std::cmp::Reverse(p.at.start));
    for p in deps {
        let value = &text[p.at.clone()];
        if value.matches(pin.as_str()).count() != 1 {
            return Err(format!("{at}: {}'s rev is spelled {value}", p.dep));
        }
        out.replace_range(p.at, &value.replacen(pin.as_str(), head, 1));
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
    fn a_move_touches_only_the_followed_urls_revs() {
        // `other` shares the rev on another repo; the comment and the
        // metadata name it too.
        let text = format!(
            "# was {A}\n{}\n[target.'cfg(unix)'.dependencies.up-unix]\ngit = \"https://github.com/sam/up\"\nrev = \"{A}\"\n\
             [dev-dependencies]\nup-dev.git = \"https://github.com/sam/up\"\nup-dev.rev = '{A}'\n\
             [workspace.metadata.notes]\nlast = \"{A}\"\n",
            manifest(A, A)
        );
        let f = &follows(&text).unwrap()[0];
        assert_eq!(f.deps, ["up-dev", "up-unix", "up-core", "up-github"]);
        let got = moved(&text, f, B).unwrap();
        let want = text
            .replace(
                &format!("sam/Up\", rev = \"{A}"),
                &format!("sam/Up\", rev = \"{B}"),
            )
            .replace(
                &format!("sam/up.git\", rev = \"{A}"),
                &format!("sam/up.git\", rev = \"{B}"),
            )
            .replace(&format!("up\"\nrev = \"{A}"), &format!("up\"\nrev = \"{B}"))
            .replace(
                &format!("up-dev.rev = '{A}'"),
                &format!("up-dev.rev = '{B}'"),
            );
        assert_eq!(got.matches(B).count(), 4, "{got}");
        assert_eq!(got, want);
        assert!(got.contains(&format!(
            "other = {{ git = \"https://github.com/sam/other\", rev = \"{A}\" }}"
        )));
        assert_eq!(follows(&got).unwrap()[0].pin, B);
        // A manifest whose pin isn't the one the follow was read at.
        let e = moved(&got, f, B).unwrap_err();
        assert!(e.contains(&format!("pinned at {B} here, not {A}")), "{e}");
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
