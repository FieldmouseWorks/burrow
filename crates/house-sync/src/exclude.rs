//! An `exclude` entry: OWNER/NAME, each side a GitHub name or a glob of one,
//! where `*` is any run of characters and `?` any one. It matches in any
//! case, as GitHub's names do, and neither wildcard reaches across the `/`.
//! An entry with anything else in it is refused when the config is read, so
//! no entry can mean something other than what it looks like.

use serde::{Deserialize, Deserializer, de};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exclude {
    owner: Vec<char>,
    name: Vec<char>,
}

impl Exclude {
    pub fn parse(entry: &str) -> Result<Exclude, String> {
        let refused = || {
            format!(
                "exclude entry {entry:?} isn't OWNER/NAME, each a name or a glob of one with * and ?"
            )
        };
        let (owner, name) = entry.split_once('/').ok_or_else(refused)?;
        let side = |s: &str| {
            let fits = !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '*' | '?'));
            fits.then(|| s.to_ascii_lowercase().chars().collect())
        };
        Ok(Exclude {
            owner: side(owner).ok_or_else(refused)?,
            name: side(name).ok_or_else(refused)?,
        })
    }

    /// Whether `repo` (OWNER/NAME) is one this entry leaves alone.
    pub fn matches(&self, repo: &str) -> bool {
        let Some((owner, name)) = repo.split_once('/').filter(|(_, n)| !n.contains('/')) else {
            return false;
        };
        let lower = |s: &str| s.to_ascii_lowercase().chars().collect::<Vec<_>>();
        glob(&self.owner, &lower(owner)) && glob(&self.name, &lower(name))
    }
}

impl<'de> Deserialize<'de> for Exclude {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Exclude, D::Error> {
        Exclude::parse(&String::deserialize(d)?).map_err(de::Error::custom)
    }
}

/// `*` any run of characters, `?` any one, anything else itself.
fn glob(pattern: &[char], text: &[char]) -> bool {
    let (mut p, mut t) = (0, 0);
    // The last star, and where in the text it's taken up to.
    let mut star = None;
    while t < text.len() {
        if pattern.get(p) == Some(&'*') {
            star = Some((p, t));
            p += 1;
        } else if pattern.get(p).is_some_and(|&c| c == '?' || c == text[t]) {
            p += 1;
            t += 1;
        } else if let Some((sp, st)) = star {
            star = Some((sp, st + 1));
            p = sp + 1;
            t = st + 1;
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod tests {
    use super::Exclude;

    fn yes(entry: &str, repo: &str) -> bool {
        Exclude::parse(entry).unwrap().matches(repo)
    }

    #[test]
    fn matches_a_name_or_a_glob_of_one_in_any_case() {
        for (entry, repo, want) in [
            ("sam/a", "sam/a", true),
            ("sam/a", "sam/ab", false),
            ("sam/a", "sam/b", false),
            ("sam/a", "kim/a", false),
            ("Sam/LoK-web", "sam/lok-WEB", true),
            ("sam/*", "sam/a", true),
            ("sam/*", "sam/a.b-c_d", true),
            ("*/a", "kim/a", true),
            ("*/a", "kim/b", false),
            ("sam/?", "sam/a", true),
            ("sam/?", "sam/ab", false),
            ("sam/?", "sam/", false),
            ("sam/a*", "sam/a", true),
            ("sam/a*b", "sam/axxb", true),
            ("sam/a*b", "sam/axxbx", false),
            ("sam/*a*a", "sam/banana", true),
            ("sam/*a*a", "sam/bananas", false),
            ("sam/a?*", "sam/a", false),
            ("sam/a?*", "sam/ab", true),
            ("s*/b", "sam/b", true),
            ("s*/b", "kim/b", false),
            ("*/*", "sam/a", true),
            // Neither wildcard reaches across the slash.
            ("sam/*", "sam/a/b", false),
            ("sam/?", "sam//", false),
            ("*/a", "sam/b/a", false),
            // `.` is itself.
            ("sam/a.b", "sam/axb", false),
        ] {
            assert_eq!(yes(entry, repo), want, "{repo} against {entry}");
        }
    }

    #[test]
    fn refuses_what_isnt_a_name_or_a_glob_of_one() {
        for entry in [
            "",
            "sam",
            "/a",
            "sam/",
            "sam/a/b",
            "sam/[ab]",
            "sam/!(a)",
            "sam/@(a|b)",
            "sam/\\*",
            "sam/a b",
            "kim/x sam/b",
            " sam/a",
            "sam/a\n",
            "sam/{a,b}",
            "sam/a^",
            "sam/é",
        ] {
            let refused = Exclude::parse(entry).unwrap_err();
            assert_eq!(
                refused,
                format!(
                    "exclude entry {entry:?} isn't OWNER/NAME, each a name or a glob of one with * and ?"
                )
            );
        }
    }
}
