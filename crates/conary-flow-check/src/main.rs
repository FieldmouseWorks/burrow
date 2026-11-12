use serde_json::Value;
use std::{env, fs};

fn keys(value: &Value, allowed: &[&str], required: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.keys().all(|key| allowed.contains(&key.as_str()))
            && required.iter().all(|key| object.contains_key(*key))
    })
}

fn nonempty(value: &Value) -> bool {
    value.as_array().is_some_and(|items| !items.is_empty())
}

fn validate(config: &Value, policy: &Value) -> bool {
    keys(
        config,
        &["settings", "trusted_authors", "plugins", "evidence"],
        &["settings", "trusted_authors", "plugins", "evidence"],
    ) && keys(
        &config["settings"],
        &["people", "reviewers", "review_authors"],
        &["people"],
    ) && keys(
        &config["plugins"],
        &["review", "fix", "judge", "proof"],
        &[],
    ) && keys(
        &config["evidence"],
        &["fresh", "negative", "require", "signer"],
        &["fresh", "negative", "require", "signer"],
    ) && nonempty(&config["trusted_authors"])
        && nonempty(&config["settings"]["people"])
        && nonempty(&config["evidence"]["require"])
        && policy
            .as_object()
            .is_some_and(|p| p.contains_key("r2") && p.contains_key("docs"))
}

fn run(args: &[String]) -> Result<(), String> {
    if args.len() != 7
        || args[0] != "flow"
        || args[1] != "compare"
        || args[2] != "--config"
        || args[4] != "--policy"
    {
        return Err("expected flow compare --config FILE --policy FILE REPO".into());
    }
    let read = |path: &str| -> Result<Value, String> {
        serde_json::from_str(&fs::read_to_string(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())
    };
    if !validate(&read(&args[3])?, &read(&args[5])?) {
        return Err("invalid flow config or policy".into());
    }
    Ok(())
}

fn main() {
    if let Err(error) = run(&env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rejects_each_schema_failure() {
        let config = json!({"settings":{"people":["sam"]},"trusted_authors":["sam"],
            "plugins":{},"evidence":{"fresh":"fresh", "negative":true,"require":["test"],"signer":"sam"}});
        let policy = json!({"r2":{},"docs":{}});
        assert!(validate(&config, &policy));
        for path in [
            vec!["extra"],
            vec!["settings", "extra"],
            vec!["plugins", "extra"],
            vec!["evidence", "extra"],
        ] {
            let mut bad = config.clone();
            let mut target = &mut bad;
            for key in &path[..path.len() - 1] {
                target = &mut target[*key];
            }
            target[path[path.len() - 1]] = json!(true);
            assert!(!validate(&bad, &policy));
        }
        for key in ["fresh", "negative", "require", "signer"] {
            let mut bad = config.clone();
            bad["evidence"].as_object_mut().unwrap().remove(key);
            assert!(!validate(&bad, &policy));
        }
        for path in [
            vec!["trusted_authors"],
            vec!["settings", "people"],
            vec!["evidence", "require"],
        ] {
            let mut bad = config.clone();
            let mut target = &mut bad;
            for key in path {
                target = &mut target[key];
            }
            *target = json!([]);
            assert!(!validate(&bad, &policy));
        }
        for key in ["r2", "docs"] {
            let mut bad = policy.clone();
            bad.as_object_mut().unwrap().remove(key);
            assert!(!validate(&config, &bad));
        }
    }

    #[test]
    fn rejects_bad_files_on_either_input() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.json");
        let policy = dir.path().join("policy.json");
        let valid_config = r#"{"settings":{"people":["sam"]},"trusted_authors":["sam"],"plugins":{},"evidence":{"fresh":"fresh","negative":true,"require":["test"],"signer":"sam"}}"#;
        let valid_policy = r#"{"r2":{},"docs":{}}"#;
        let args: Vec<String> = [
            "flow",
            "compare",
            "--config",
            config.to_str().unwrap(),
            "--policy",
            policy.to_str().unwrap(),
            "repo",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        for broken in [&config, &policy] {
            for contents in [None, Some("{"), Some("null")] {
                fs::write(&config, valid_config).unwrap();
                fs::write(&policy, valid_policy).unwrap();
                assert!(run(&args).is_ok());
                if let Some(text) = contents {
                    fs::write(broken, text).unwrap();
                } else {
                    fs::remove_file(broken).unwrap();
                }
                assert!(run(&args).is_err());
            }
        }
    }

    #[test]
    fn rejects_invalid_arguments() {
        for args in [
            vec![],
            vec!["flow"],
            vec!["flow", "compare", "--other", "a", "--policy", "b", "repo"],
        ] {
            assert!(run(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
    }
}
