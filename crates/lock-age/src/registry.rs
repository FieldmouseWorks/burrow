//! The world as it is: crates.io, npm's registry, cargo and npm, and
//! release-ages.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use jiff::Timestamp;
use serde_json::Value;
use ureq::tls::{RootCerts, TlsConfig};

use crate::{Ages, NPM, Out, Release, World, when};

const UA: &str = "burrow lock-age (https://github.com/FieldmouseWorks/burrow)";

pub struct Real {
    /// release-ages, as a command that reads a config on its stdin.
    resolver: Vec<String>,
    agent: ureq::Agent,
    /// When crates.io's API was last asked: it wants at most a request a
    /// second.
    asked: Option<Instant>,
    index: BTreeMap<String, Vec<Release>>,
    npm: BTreeMap<String, BTreeMap<String, String>>,
}

impl Real {
    pub fn new(resolver: Vec<String>) -> Real {
        // The machine's own trust (its CA bundle), as every other program
        // on it has.
        let tls = TlsConfig::builder()
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .tls_config(tls)
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(UA)
            .build()
            .into();
        Real {
            resolver,
            agent,
            asked: None,
            index: BTreeMap::new(),
            npm: BTreeMap::new(),
        }
    }

    fn get(&self, url: &str, accept: &str) -> Result<String, String> {
        self.agent
            .get(url)
            .header("Accept", accept)
            .call()
            .and_then(|mut r| r.body_mut().with_config().limit(u64::MAX).read_to_string())
            .map_err(|e| format!("{url}: {e}"))
    }

    fn get_json(&self, url: &str) -> Result<Value, String> {
        let text = self.get(url, "application/json")?;
        serde_json::from_str(&text).map_err(|e| format!("{url}: {e}"))
    }
}

/// Where a crate's entry is in the sparse index.
pub fn index_path(name: &str) -> String {
    let n = name.to_lowercase();
    match n.len() {
        0..=2 => format!("{}/{n}", n.len()),
        3 => format!("3/{}/{n}", &n[..1]),
        _ => format!("{}/{}/{n}", &n[..2], &n[2..4]),
    }
}

/// A URL's path segment, `safe` kept as it is, as Python's quote does.
fn quote(s: &str, safe: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"_.-~".contains(&b) || safe.as_bytes().contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl World for Real {
    fn crate_index(&mut self, name: &str) -> Result<Vec<Release>, String> {
        if let Some(got) = self.index.get(name) {
            return Ok(got.clone());
        }
        let text = self.get(
            &format!("https://index.crates.io/{}", index_path(name)),
            "*/*",
        )?;
        let mut got = vec![];
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let x: Value =
                serde_json::from_str(line).map_err(|e| format!("{name}'s index: {e}"))?;
            got.push(Release {
                vers: x["vers"].as_str().unwrap_or_default().to_owned(),
                pubtime: x["pubtime"].as_str().map(String::from),
                yanked: x["yanked"].as_bool().unwrap_or(false),
            });
        }
        self.index.insert(name.to_owned(), got.clone());
        Ok(got)
    }

    fn crate_created(&mut self, name: &str, version: &str) -> Result<Timestamp, String> {
        if let Some(wait) = self
            .asked
            .and_then(|at| Duration::from_secs(1).checked_sub(at.elapsed()))
        {
            std::thread::sleep(wait);
        }
        self.asked = Some(Instant::now());
        let url = format!(
            "https://crates.io/api/v1/crates/{}/{}",
            quote(name, "/"),
            quote(version, "/")
        );
        let got = self.get_json(&url)?;
        match got["version"]["created_at"].as_str() {
            Some(t) => when(t),
            None => Err(format!("{url}: no created_at")),
        }
    }

    fn npm_times(&mut self, name: &str) -> Result<BTreeMap<String, String>, String> {
        if let Some(got) = self.npm.get(name) {
            return Ok(got.clone());
        }
        let got = self.get_json(&format!("{NPM}{}", quote(name, "@")))?;
        let times: BTreeMap<String, String> = got["time"]
            .as_object()
            .ok_or(format!("the registry has no publish times for {name}"))?
            .iter()
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect();
        self.npm.insert(name.to_owned(), times.clone());
        Ok(times)
    }

    fn run(&mut self, argv: &[&str], dir: &Path) -> Out {
        match Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(dir)
            .stdin(Stdio::null())
            .output()
        {
            Ok(o) => Out {
                code: o.status.code().unwrap_or(-1),
                stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&o.stderr).into_owned(),
            },
            Err(e) => Out {
                code: -1,
                stdout: String::new(),
                stderr: format!("{}: {e}", argv[0]),
            },
        }
    }

    fn release_ages(&mut self, config: &Value) -> Result<Ages, String> {
        release_ages(&self.resolver, config)
    }

    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// What release-ages says of a config: the ages it sets (presets resolved),
/// its configFileNames (only when set, null included), inheritConfig and
/// forkMode.
pub fn release_ages(resolver: &[String], config: &Value) -> Result<Ages, String> {
    let failed = |why: &str| format!("can't tell the release ages: {why}");
    let (cmd, args) = resolver
        .split_first()
        .ok_or_else(|| failed("no release-ages"))?;
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| failed(&format!("{cmd}: {e}")))?;
    let mut stdin = child.stdin.take().expect("piped");
    let wrote = stdin.write_all(config.to_string().as_bytes());
    drop(stdin);
    let out = child
        .wait_with_output()
        .map_err(|e| failed(&e.to_string()))?;
    if !out.status.success() || wrote.is_err() {
        let err = String::from_utf8_lossy(&out.stderr);
        let why = err
            .lines()
            .rfind(|l| l.contains("Error") || l.contains("validationError"))
            .unwrap_or("failed");
        return Err(failed(why.trim()));
    }
    let got: Value = serde_json::from_slice(&out.stdout).map_err(|e| failed(&e.to_string()))?;
    let ages = got["ages"]
        .as_array()
        .ok_or_else(|| failed("no ages"))?
        .iter()
        .map(|x| {
            x.as_f64()
                .ok_or_else(|| failed("an age that isn't a number"))
        })
        .collect::<Result<_, _>>()?;
    Ok(Ages {
        ages,
        config_file_names: got.get("configFileNames").cloned(),
        inherit_config: got["inheritConfig"].as_bool().unwrap_or(false),
        fork_mode: got["forkMode"].as_bool().unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sh(script: &str) -> Vec<String> {
        ["sh", "-c", script].map(String::from).to_vec()
    }

    #[test]
    fn reads_release_ages_answer_or_its_failure() {
        let answer = r#"{"ages": [86400000, 1000], "inheritConfig": false, "forkMode": false}"#;
        let got =
            release_ages(&sh(&format!("cat >/dev/null; echo '{answer}'")), &json!({})).unwrap();
        assert_eq!(
            got,
            Ages {
                ages: vec![86_400_000.0, 1000.0],
                ..Ages::default()
            }
        );
        let got = release_ages(
            &sh(r#"cat >/dev/null; echo '{"ages": [], "configFileNames": null}'"#),
            &json!({}),
        );
        assert_eq!(got.unwrap().config_file_names, Some(Value::Null));
        let err =
            release_ages(&sh("echo 'Error: no such preset' >&2; exit 1"), &json!({})).unwrap_err();
        assert!(err.ends_with("Error: no such preset"), "{err}");
        // What it's given is the config, whole.
        let got = release_ages(
            &sh(r#"jq -c '{ages: [.n], inheritConfig: .i}'"#),
            &json!({"n": 5, "i": true}),
        );
        assert_eq!(
            got.unwrap(),
            Ages {
                ages: vec![5.0],
                inherit_config: true,
                ..Ages::default()
            }
        );
    }

    #[test]
    fn names_the_registrys_paths() {
        assert_eq!(index_path("A"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("Abc"), "3/a/abc");
        assert_eq!(index_path("serde_json"), "se/rd/serde_json");
        assert_eq!(quote("@s/k", "@"), "@s%2Fk");
        assert_eq!(quote("1.0.0+build", "/"), "1.0.0%2Bbuild");
    }
}
