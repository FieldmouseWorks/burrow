//! Who lands each repo's PRs and the proofs that run on a machine rather
//! than in CI, each with how.
//!
//! For a repo burrow-landing's record gives conaryd (or is handing over to
//! it), coverage comes directly from the record (`Landing::By("conaryd")`)
//! and its proofs from its flow configuration (`from_flow`). For a repo
//! the steward holds, both come from the steward's repos.json (`Steward`),
//! read as the script's jq read it: its repos matched without their case,
//! and their proofs in the order the file has them.

use std::fmt;
use std::fs;
use std::path::Path;

use conary_github::flow::{self, Recipe};
use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::Value;

/// How long the steward's list is believed: past it, the steward has
/// stopped, which is the stall watchdog's to take up, and who lands what
/// isn't known.
const BELIEVED: f64 = 6.0 * 3600.0;

/// A JSON value whose objects keep their entries in the file's order, as
/// jq keeps them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum Json {
    Object(Entries),
    Other(Value),
}

/// An object's entries, in order.
#[derive(Debug)]
pub(crate) struct Entries(Vec<(String, Json)>);

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Entries, D::Error> {
        struct InOrder;
        impl<'de> Visitor<'de> for InOrder {
            type Value = Entries;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Entries(entries))
            }
        }
        d.deserialize_map(InOrder)
    }
}

impl Json {
    /// Its entry `key` (the last, as jq keeps one), if it's an object.
    fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(Entries(e)) => e.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v),
            Json::Other(_) => None,
        }
    }

    /// As a plain value, the way jq's string interpolation shows it.
    fn value(&self) -> Value {
        match self {
            Json::Object(Entries(e)) => {
                Value::Object(e.iter().map(|(k, v)| (k.clone(), v.value())).collect())
            }
            Json::Other(v) => v.clone(),
        }
    }
}

/// The steward's repos.json, as far as it can be read.
#[derive(Debug)]
pub(crate) enum Steward {
    /// The machine has no steward: nothing lands any repo's PRs.
    None,
    /// It has one, whose list isn't there or isn't JSON.
    Unread,
    /// Its list.
    Read(Json),
}

/// Who lands a repo's PRs: whoever does merges an ok major once its CI is
/// green, so only one nothing lands is the owner's.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) enum Landing {
    /// Nothing: no steward, or one that doesn't have the repo.
    Nobody,
    /// The steward or conaryd, by its name.
    By(String),
    /// Not known: the list can't be read or is too old, or its landing
    /// record couldn't be read when it was written.
    #[default]
    Unknown,
}

/// What the steward says of one repo.
#[derive(Debug, Default)]
pub(crate) struct Repo {
    /// Who lands its PRs.
    pub landing: Landing,
    /// The names of the proofs it runs on a machine there, as values to
    /// match a check's name against, in jq's `keys` order.
    pub proof_names: Vec<Value>,
    /// How each proof runs, for the job: " NAME isn't CI: …" each, in the
    /// file's order.
    pub proofs: String,
}

impl Steward {
    /// The file at `path`, when there's a steward (a path).
    pub fn read(path: Option<&Path>) -> Steward {
        let Some(path) = path else {
            return Steward::None;
        };
        fs::read(path)
            .ok()
            .and_then(|text| serde_json::from_slice(&text).ok())
            .map_or(Steward::Unread, Steward::Read)
    }

    /// What it says of `repo`, who lands it believed if its list is under
    /// six hours old at `now`. A list that doesn't have it, fresh, has
    /// nothing land it; what jq couldn't read names no proofs there, and
    /// who lands it isn't known.
    pub fn repo(&self, repo: &str, now: i64) -> Repo {
        let file = match self {
            Steward::None => {
                return Repo {
                    landing: Landing::Nobody,
                    ..Repo::default()
                };
            }
            Steward::Unread => return Repo::default(),
            Steward::Read(file) => file,
        };
        let fresh = match file.get("at") {
            Some(Json::Other(Value::Number(at))) => {
                at.as_f64().is_some_and(|at| (now as f64) - at < BELIEVED)
            }
            _ => false,
        };
        // jq's `.repos | with_entries(.key |= ascii_downcase) | .[$r | ascii_downcase]`.
        let want = repo.to_ascii_lowercase();
        let entry = match file.get("repos") {
            Some(Json::Object(Entries(e))) => e
                .iter()
                .rev()
                .find(|(k, _)| k.to_ascii_lowercase() == want)
                .map(|(_, v)| v),
            _ => return Repo::default(),
        };
        let Some(entry) = entry else {
            let landing = if fresh {
                Landing::Nobody
            } else {
                Landing::Unknown
            };
            return Repo {
                landing,
                ..Repo::default()
            };
        };
        let Json::Object(_) = entry else {
            return Repo::default();
        };
        let landing = match entry.get("lands") {
            Some(Json::Other(Value::String(by))) if fresh && !by.is_empty() => {
                Landing::By(by.clone())
            }
            _ => Landing::Unknown,
        };
        let (proof_names, proofs) = match entry.get("proofs") {
            Some(Json::Object(Entries(e))) => {
                let mut names: Vec<&String> = e.iter().map(|(k, _)| k).collect();
                names.sort();
                names.dedup();
                let how = e
                    .iter()
                    .map(|(name, how)| {
                        format!(
                            " {name} isn't CI: it runs on a machine, like this: {} Rerun it that way on what you push.",
                            super::shown(Some(&how.value()))
                        )
                    })
                    .collect();
                (
                    names
                        .into_iter()
                        .map(|n| Value::String(n.clone()))
                        .collect(),
                    how,
                )
            }
            _ => (Vec::new(), String::new()),
        };
        Repo {
            landing,
            proof_names,
            proofs,
        }
    }
}

/// Coverage and proofs for a repo the landing record gives conaryd: it's
/// landed by conaryd, and its proofs come from its flow configuration
/// (`OWNER/NAME.json`, lowercased) under `flow`, when a flow directory is
/// configured.
pub(crate) fn from_flow(flow: Option<&Path>, repo: &str) -> Result<Repo, String> {
    let Some(dir) = flow else {
        return Ok(Repo {
            landing: Landing::By("conaryd".into()),
            ..Repo::default()
        });
    };
    let path = dir.join(format!("{}.json", repo.to_lowercase()));
    let text = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let cfg: flow::Config =
        serde_json::from_slice(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut proof_names = Vec::with_capacity(cfg.settings.proofs.len());
    let mut proofs = String::new();
    for (name, recipe) in &cfg.settings.proofs {
        let how =
            describe_recipe(name, recipe).map_err(|why| format!("{}: {why}", path.display()))?;
        proof_names.push(Value::String(name.clone()));
        proofs.push_str(&format!(
            " {name} isn't CI: it runs on a machine, like this: {how} Rerun it that way on what you push."
        ));
    }
    Ok(Repo {
        landing: Landing::By("conaryd".into()),
        proof_names,
        proofs,
    })
}

/// How a job reruns a flow proof recipe on what it pushes.
fn describe_recipe(name: &str, recipe: &Recipe) -> Result<String, String> {
    match recipe {
        Recipe::Job(how) => Ok(how.clone()),
        Recipe::Build(attr) => Ok(format!(
            "`nix build .#{attr} -L` on the commit. conaryd builds it itself for each new head and posts the `{name}` check: never post that check yourself."
        )),
        Recipe::Run(argv) => {
            if argv.is_empty() {
                return Err(format!("proof {name} has no command"));
            }
            let cmd: Vec<String> = argv.iter().map(|w| shell_quote(w)).collect();
            Ok(format!(
                "`{}` in a worktree at the commit, with PROOF_REPO, PROOF_HEAD and PROOF_CHECK set. conaryd runs it itself for each new head and posts the `{name}` check: never post that check yourself.",
                cmd.join(" ")
            ))
        }
    }
}

/// A POSIX shell word, quoted only when it needs it (as Python's `shlex.quote`).
fn shell_quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(&b))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\"'\"'"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steward(text: &str) -> Steward {
        serde_json::from_str(text).map_or(Steward::Unread, Steward::Read)
    }

    fn by(who: &str) -> Landing {
        Landing::By(who.into())
    }

    #[test]
    fn a_repo_is_matched_without_its_case_and_its_proofs_kept_in_order() {
        let s = steward(
            r#"{"at": 1000, "repos": {"Sam/R": {"lands": "steward", "proofs": {"z-proof": "ssh z", "a-proof": {"run": 1}}}}}"#,
        );
        let r = s.repo("sam/r", 1000 + 3600);
        assert_eq!(r.landing, by("steward"));
        assert_eq!(
            r.proof_names,
            [Value::from("a-proof"), Value::from("z-proof")]
        );
        assert_eq!(
            r.proofs,
            " z-proof isn't CI: it runs on a machine, like this: ssh z Rerun it that way on what you push. \
             a-proof isn't CI: it runs on a machine, like this: {\"run\":1} Rerun it that way on what you push."
        );
        // Six hours on, who lands it isn't known: the steward may have
        // stopped, which is the watchdog's. Its proofs still run as they did.
        let stale = s.repo("sam/r", 1000 + 6 * 3600);
        assert_eq!(stale.landing, Landing::Unknown);
        assert_eq!(stale.proof_names.len(), 2);
    }

    #[test]
    fn only_a_list_that_lacks_the_repo_or_no_steward_has_nothing_land_it() {
        // remi's list of 2026-11-24 02:20, from a pass that yielded partway
        // through burrow: who lands each repo, however far it got.
        let s = steward(
            r#"{"at": 1000, "repos": {"FieldmouseWorks/burrow": {"lands": "steward", "proofs": {}},
                "FieldmouseWorks/remi": {"lands": "conaryd", "proofs": {}}}}"#,
        );
        assert_eq!(
            s.repo("fieldmouseworks/burrow", 1000).landing,
            by("steward")
        );
        assert_eq!(s.repo("FieldmouseWorks/remi", 1000).landing, by("conaryd"));
        assert_eq!(s.repo("sam/other", 1000).landing, Landing::Nobody);
        assert_eq!(
            s.repo("sam/other", 1000 + 6 * 3600).landing,
            Landing::Unknown
        );
        assert_eq!(
            Steward::read(None).repo("sam/r", 0).landing,
            Landing::Nobody
        );
        let gone = Path::new("/nonexistent/repos.json");
        assert_eq!(
            Steward::read(Some(gone)).repo("sam/r", 0).landing,
            Landing::Unknown
        );
    }

    #[test]
    fn what_it_cant_read_says_nobody_lands_nothing_and_names_no_proofs() {
        for text in [
            "not json",
            r#"{"at": 1000}"#,
            r#"{"at": "1000", "repos": {"sam/r": {"lands": "steward"}}}"#,
            // The list before who lands each repo was in it.
            r#"{"at": 1000, "repos": {"sam/r": {"covered": false}}}"#,
            r#"{"at": 1000, "repos": {"sam/r": {"lands": null}}}"#,
            r#"{"at": 1000, "repos": {"sam/r": {"lands": true}}}"#,
            r#"{"at": 1000, "repos": {"sam/r": true}}"#,
            r#"{"at": 1000, "repos": {"sam/r": {"lands": "steward", "proofs": null}}}"#,
        ] {
            let r = steward(text).repo("sam/r", 1000);
            assert!(r.proof_names.is_empty() && r.proofs.is_empty(), "{text}");
            if !text.ends_with("null}}}") || text.contains("\"lands\": null") {
                assert_eq!(r.landing, Landing::Unknown, "{text}");
            }
        }
        // Two spellings of one repo: the last is the one.
        let s = steward(
            r#"{"at": 1000, "repos": {"sam/r": {"lands": "steward"}, "SAM/R": {"lands": null}}}"#,
        );
        assert_eq!(s.repo("sam/r", 1000).landing, Landing::Unknown);
    }

    #[test]
    fn a_conaryd_repo_reads_its_proofs_from_its_flow_config() {
        // Without a flow directory, it's still covered by conaryd with no proofs.
        let bare = from_flow(None, "Sam/R").unwrap();
        assert_eq!(bare.landing, by("conaryd"));
        assert!(bare.proof_names.is_empty() && bare.proofs.is_empty());

        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("sam")).unwrap();
        fs::write(
            dir.path().join("sam/r.json"),
            serde_json::json!({
                "settings": {
                    "proofs": {
                        "z-job": {"Job": "Run `just hil` on the bench box."},
                        "a-build": {"Build": "checks.x86_64-linux.agents"},
                        "m-run": {"Run": ["/bin/bash", "-c", "echo 'hi' && make gate"]},
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        let r = from_flow(Some(dir.path()), "Sam/R").unwrap();
        assert_eq!(r.landing, by("conaryd"));
        assert_eq!(
            r.proof_names,
            [
                Value::from("a-build"),
                Value::from("m-run"),
                Value::from("z-job"),
            ]
        );
        assert_eq!(
            r.proofs,
            " a-build isn't CI: it runs on a machine, like this: `nix build .#checks.x86_64-linux.agents -L` on the commit. conaryd builds it itself for each new head and posts the `a-build` check: never post that check yourself. Rerun it that way on what you push. \
             m-run isn't CI: it runs on a machine, like this: `/bin/bash -c 'echo '\"'\"'hi'\"'\"' && make gate'` in a worktree at the commit, with PROOF_REPO, PROOF_HEAD and PROOF_CHECK set. conaryd runs it itself for each new head and posts the `m-run` check: never post that check yourself. Rerun it that way on what you push. \
             z-job isn't CI: it runs on a machine, like this: Run `just hil` on the bench box. Rerun it that way on what you push."
        );

        // Missing, malformed, or an empty Run command fails closed.
        assert!(from_flow(Some(dir.path()), "sam/missing").is_err());
        fs::write(dir.path().join("sam/bad.json"), "not json").unwrap();
        assert!(from_flow(Some(dir.path()), "sam/bad").is_err());
        fs::write(
            dir.path().join("sam/empty.json"),
            serde_json::json!({"settings": {"proofs": {"gate": {"Run": []}}}}).to_string(),
        )
        .unwrap();
        assert!(from_flow(Some(dir.path()), "sam/empty").is_err());
    }
}
