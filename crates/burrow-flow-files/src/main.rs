//! `burrow-flow-files flow|policy|events|producers FILE...`: each file read
//! as the flow, `cn policy` or conaryd reads it, with Conary's own loaders
//! (`flow::Config::load`, `Policy::load`, `flow::schema::load` of conaryd's
//! events file, `events::Runtime`, and the flow's producer keys'
//! `Producers::load`), so a key Conary doesn't know, a value of the wrong
//! type or a malformed file is refused here as it would be by the machine's
//! cn. burrow's checks have no Conary flake, so this is how they hold the
//! files its modules render to Conary's types, and the machine's build
//! holds its producers file to it: `cn flow settings` reads the flow
//! config, not the producers file it names.
//!
//! `burrow-flow-files flow --stand-in-keys FILE...` loads each as the flow
//! does, but with every seal key it names by its file (`*_key_file`) read
//! from a stand-in key instead: a build has none of the machine's key files,
//! only the absolute paths the configs name them by, which it holds them to.
//!
//! `burrow-flow-files render MANIFEST` writes each repo's flow config as
//! the flow reads it on the machine: its base, as the build rendered it,
//! with each role-run plugin's choice (`execution`) as the settings say now
//! ([`render`]).
//!
//! `burrow-flow-files key judge|major CONFIG SEAL` loads CONFIG as the flow
//! does (`flow::Config::load`, which reads a key its `judge_key_file` or
//! `major_key_file` names) and succeeds only if its `judge_key` (the
//! judge's seal key) or `major_key` (renovate-sweep's) is the key the file
//! SEAL holds, printing neither.
//!
//! `burrow-flow-files schema` prints the flow config's schema as the pinned
//! Conary publishes it (`flow::schema::flow`, what `cn flow settings
//! --schema` prints as its `flow`), so a check can hold what the machine
//! renders to every setting the pin has, not to a list written by hand.
use conary_core::Policy;
use conary_github::attest::service::Producers;
use conary_github::events::Runtime;
use conary_github::flow::{Config, SealKey};
use std::path::Path;
use std::process::ExitCode;

mod render;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let kind = args.next();
    let mut files: Vec<String> = args.collect();
    let stand_in =
        kind.as_deref() == Some("flow") && files.first().is_some_and(|f| f == "--stand-in-keys");
    if stand_in {
        files.remove(0);
    }
    let read: fn(&Path) -> Result<(), String> = match kind.as_deref() {
        Some("flow") if stand_in => stand_in_keys,
        Some("flow") => |p| Config::load(p).map(drop).map_err(|e| e.to_string()),
        Some("policy") => |p| Policy::load(p).map(drop).map_err(|e| e.to_string()),
        Some("events") => |p| {
            let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
            conary_github::flow::schema::load::<Runtime>(&bytes)
                .map(drop)
                .map_err(|e| e.to_string())
        },
        Some("producers") => |p| Producers::load(p).map(drop),
        Some("render") => return render(&files),
        Some("key") => return key(&files),
        Some("schema") if files.is_empty() => return schema(),
        _ => return usage(),
    };
    if files.is_empty() {
        return usage();
    }
    let mut ok = true;
    for file in &files {
        if let Err(e) = read(Path::new(file)) {
            eprintln!("{file}: {e}");
            ok = false;
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn render(args: &[String]) -> ExitCode {
    let [manifest] = args else {
        return usage();
    };
    let read = std::fs::read(manifest)
        .map_err(|e| format!("{manifest}: {e}"))
        .and_then(|bytes| {
            serde_json::from_slice::<render::Manifest>(&bytes)
                .map_err(|e| format!("{manifest}: {e}"))
        })
        .and_then(|m| render::render(&m).map(|n| (n, m.out)));
    match read {
        Ok((n, out)) => {
            println!("rendered {n} flow configs in {}", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn key(args: &[String]) -> ExitCode {
    let [which, config, seal] = args else {
        return usage();
    };
    let (name, pick): (&str, fn(Config) -> Option<SealKey>) = match which.as_str() {
        "judge" => ("judge_key", |c| c.settings.judge_key),
        "major" => ("major_key", |c| c.settings.major_key),
        _ => return usage(),
    };
    let loaded = Config::load(Path::new(config))
        .map(pick)
        .map_err(|e| format!("{config}: {e}"));
    let sealed = SealKey::read(Path::new(seal));
    match (loaded, sealed) {
        (Ok(Some(loaded)), Ok(sealed)) if loaded == sealed => ExitCode::SUCCESS,
        (Ok(Some(_)), Ok(_)) => {
            eprintln!("{config}: its {name} isn't the one {seal} holds");
            ExitCode::FAILURE
        }
        (Ok(None), _) => {
            eprintln!("{config}: it has no {name}");
            ExitCode::FAILURE
        }
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

/// The flow config at `path`, loaded as the flow loads it with each key it
/// names by its file read from a stand-in key: the file named must be an
/// absolute path, as the machine's are.
fn stand_in_keys(path: &Path) -> Result<(), String> {
    let text = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut config: serde_json::Value = serde_json::from_slice(&text).map_err(|e| e.to_string())?;
    let dir = tempfile::tempdir().map_err(|e| e.to_string())?;
    let key = dir.path().join("stand-in.key");
    std::fs::write(&key, "0".repeat(64)).map_err(|e| e.to_string())?;
    if let Some(settings) = config.get_mut("settings").and_then(|s| s.as_object_mut()) {
        for (name, file) in settings
            .iter_mut()
            .filter(|(name, _)| name.ends_with("_key_file"))
        {
            match file {
                serde_json::Value::Null => {}
                serde_json::Value::String(named) if Path::new(named).is_absolute() => {
                    *file = key.to_string_lossy().into_owned().into();
                }
                _ => return Err(format!("settings.{name} isn't an absolute path: {file}")),
            }
        }
    }
    let copy = dir.path().join("config.json");
    std::fs::write(&copy, config.to_string()).map_err(|e| e.to_string())?;
    Config::load(&copy).map(drop).map_err(|e| e.to_string())
}

fn schema() -> ExitCode {
    match conary_github::flow::schema::flow() {
        Ok(schema) => {
            println!("{schema}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("the flow's schema: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: burrow-flow-files flow [--stand-in-keys]|policy|events|producers FILE... | burrow-flow-files render MANIFEST | burrow-flow-files key judge|major CONFIG SEAL | burrow-flow-files schema"
    );
    ExitCode::from(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config naming its keys by `files` (each a settings key and its
    /// value), at a file of its own in `dir`.
    fn config(dir: &Path, files: serde_json::Value) -> std::path::PathBuf {
        let path = dir.join(format!("config-{}.json", files.to_string().len()));
        std::fs::write(&path, serde_json::json!({ "settings": files }).to_string()).unwrap();
        path
    }

    #[test]
    fn a_stand_in_reads_the_keys_a_build_lacks_and_holds_their_paths_to_the_machines() {
        let dir = tempfile::tempdir().unwrap();
        let named = config(
            dir.path(),
            serde_json::json!({"major_key_file": "/run/autoland-flow-keys/major-seal", "judge_key_file": "/run/autoland-flow-keys/judge-seal"}),
        );
        // As the flow loads it, the key files it names aren't here.
        assert!(Config::load(&named).is_err());
        stand_in_keys(&named).unwrap();
        stand_in_keys(&config(dir.path(), serde_json::json!({}))).unwrap();
        // A path the machine couldn't name a key by, or none at all.
        for files in [
            serde_json::json!({"major_key_file": "major-seal"}),
            serde_json::json!({"judge_key_file": 1}),
        ] {
            assert!(
                stand_in_keys(&config(dir.path(), files.clone())).is_err(),
                "{files}"
            );
        }
        // Everything else is Conary's to refuse, as ever: a key it doesn't
        // know, or a key inline beside its file.
        for files in [
            serde_json::json!({"major_key_file": "/run/k", "bogus": 1}),
            serde_json::json!({"major_key_file": "/run/k", "major_key": "0".repeat(64)}),
        ] {
            assert!(
                stand_in_keys(&config(dir.path(), files.clone())).is_err(),
                "{files}"
            );
        }
    }
}
