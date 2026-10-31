//! The machine's nix, as burrow-keep has it read the store and write the
//! cache: `nix` and `nix-store` on PATH, on the store nix's config names.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::{Info, Nix, Store};

pub struct Real;

/// Derivations per `nix-store --query --outputs`, well inside what a
/// command line holds.
const CHUNK: usize = 1000;

/// Run it, with this on its stdin: what it printed, or why it failed.
fn run(cmd: &mut Command, input: &str) -> Result<Vec<u8>, String> {
    let what = format!("{cmd:?}");
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| format!("{what}: {e}"))?;
    let mut stdin = child.stdin.take().expect("a piped stdin");
    stdin
        .write_all(input.as_bytes())
        .map_err(|e| format!("{what}: {e}"))?;
    drop(stdin);
    let out = child
        .wait_with_output()
        .map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(format!("{what}: {}", out.status))
    }
}

fn nix() -> Command {
    let mut cmd = Command::new("nix");
    cmd.args(["--extra-experimental-features", "nix-command"]);
    cmd
}

/// The store from `nix path-info --all --json --json-format 2`.
pub fn parse_store(json: &[u8]) -> Result<Store, String> {
    let v: Value = serde_json::from_slice(json).map_err(|e| format!("nix path-info: {e}"))?;
    let dir = v["storeDir"]
        .as_str()
        .ok_or("nix path-info: no storeDir")?
        .to_owned();
    let info = v["info"].as_object().ok_or("nix path-info: no info")?;
    let mut paths = BTreeMap::new();
    for (name, p) in info {
        let field = |key: &str| {
            let f = &p[key];
            (!f.is_null())
                .then_some(f)
                .ok_or_else(|| format!("nix path-info: {name} has no {key}"))
        };
        let signatures = field("signatures")?.as_array();
        let references = field("references")?.as_array();
        let (Some(signatures), Some(references), Some(nar_hash), Some(nar_size)) = (
            signatures,
            references,
            field("narHash")?.as_str(),
            field("narSize")?.as_u64(),
        ) else {
            return Err(format!("nix path-info: {name}: {p}"));
        };
        let references = references
            .iter()
            .map(|r| r.as_str().map(|r| format!("{dir}/{r}")))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| format!("nix path-info: {name}: {p}"))?;
        let info = Info {
            signed: !signatures.is_empty(),
            references,
            nar_hash: nar_hash.to_owned(),
            nar_size,
        };
        paths.insert(format!("{dir}/{name}"), info);
    }
    Ok(Store { dir, paths })
}

impl Nix for Real {
    fn store(&self) -> Result<Store, String> {
        let out = run(
            nix().args(["path-info", "--all", "--json", "--json-format", "2"]),
            "",
        )?;
        parse_store(&out)
    }

    fn outputs(&self, drvs: &[String]) -> Result<BTreeSet<String>, String> {
        let mut made = BTreeSet::new();
        for chunk in drvs.chunks(CHUNK) {
            let out = run(
                Command::new("nix-store")
                    .args(["--query", "--outputs"])
                    .args(chunk),
                "",
            )?;
            made.extend(String::from_utf8_lossy(&out).lines().map(str::to_owned));
        }
        Ok(made)
    }

    fn copy(&self, paths: &[String], to: &Path) -> Result<(), String> {
        let to = format!("file://{}?compression=none", to.display());
        run(
            nix().args(["copy", "--to", &to, "--stdin"]),
            &paths.join("\n"),
        )
        .map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::parse_store;

    /// nix's path-info, as 2.34 writes it: names without the store's
    /// directory, which it gives once.
    #[test]
    fn the_store_is_each_path_what_it_refers_to_and_whether_anyone_signed_it() {
        let s = parse_store(
            br#"{"info": {
                "aaa-built": {"signatures": [], "narHash": "sha256-A=", "narSize": 1,
                    "references": ["aaa-built", "bbb-public"], "version": 2},
                "bbb-public": {"signatures": ["cache.nixos.org-1:x"], "narHash": "sha256-B=",
                    "narSize": 2, "references": [], "version": 2}
            }, "storeDir": "/nix/store", "version": 2}"#,
        )
        .unwrap();
        assert_eq!(s.dir, "/nix/store");
        let got: Vec<_> = s
            .paths
            .iter()
            .map(|(p, i)| {
                (
                    p.as_str(),
                    i.signed,
                    i.references.clone(),
                    i.nar_hash.as_str(),
                    i.nar_size,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                (
                    "/nix/store/aaa-built",
                    false,
                    vec![
                        "/nix/store/aaa-built".to_owned(),
                        "/nix/store/bbb-public".to_owned()
                    ],
                    "sha256-A=",
                    1
                ),
                ("/nix/store/bbb-public", true, vec![], "sha256-B=", 2)
            ]
        );
        let path = |fields: &str| {
            format!(r#"{{"info": {{"aaa-x": {{{fields}}}}}, "storeDir": "/nix/store"}}"#)
        };
        let whole = r#""signatures": [], "references": [], "narHash": "sha256-A=", "narSize": 1"#;
        assert!(parse_store(path(whole).as_bytes()).is_ok());
        let mut bad = vec![
            r#"{"info": {}}"#.to_owned(),
            r#"{"storeDir": "/nix/store"}"#.to_owned(),
            "nope".to_owned(),
            path(&whole.replace(r#""references": []"#, r#""references": [1]"#)),
            path(&whole.replace(r#""narSize": 1"#, r#""narSize": "1""#)),
        ];
        for field in ["signatures", "references", "narHash", "narSize"] {
            let kept: Vec<&str> = whole.split(", ").filter(|f| !f.contains(field)).collect();
            bad.push(path(&kept.join(", ")));
        }
        for bad in bad {
            assert!(parse_store(bad.as_bytes()).is_err(), "{bad}");
        }
    }
}
