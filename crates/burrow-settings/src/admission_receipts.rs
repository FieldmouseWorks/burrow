//! The previous admission writer used exact supplied paths. Keep its format and
//! lookup here so upgrade/rollback tests cannot silently inherit new semantics.
use super::*;
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

#[derive(Default, Deserialize, Serialize)]
#[serde(transparent)]
struct Legacy(Overlay);
impl live::Document for Legacy {
    fn receipts(&mut self) -> &mut BTreeMap<String, String> {
        &mut self.0.receipts
    }
}

fn words(command: &str) -> Vec<String> {
    command.split_whitespace().map(str::to_owned).collect()
}

fn legacy_request(cfg: &Config, receipt: &Path, command: &str) -> String {
    live::write(
        &cfg.overlay,
        &cfg.overlay.with_extension("lock"),
        &cfg.overlay.with_extension("log"),
        "previous generation",
        Some(&receipt.to_string_lossy()),
        &words(command),
        |state: &mut Legacy| {
            let args = words(command);
            match args[0].as_str() {
                "allow" | "pause" => {
                    state.0.repos.insert(args[1].clone(), args[0] == "allow");
                }
                "pipeline" => state.0.pipeline_until = Some(args[1].parse().unwrap()),
                "back" => state.0.pipeline_until = None,
                _ => unreachable!(),
            }
            Ok(Some("original admission answer".into()))
        },
    )
    .unwrap()
    .unwrap()
}

fn paths(dir: &Path) -> Vec<PathBuf> {
    let state = dir.join("state");
    fs::create_dir(&state).unwrap();
    fs::create_dir(state.join("child")).unwrap();
    let alias = dir.join("alias");
    std::os::unix::fs::symlink(&state, &alias).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let depth = cwd
        .components()
        .filter(|c| matches!(c, Component::Normal(_)))
        .count();
    let relative = PathBuf::from("../".repeat(depth)).join(state.strip_prefix("/").unwrap());
    [state.clone(), alias, relative, state.join("child/../.")]
        .into_iter()
        .map(|p| p.join("open-cycle"))
        .collect()
}

fn cases() -> Vec<(String, String)> {
    vec![
        ("allow sam/box".into(), "pause sam/box".into()),
        ("pause sam/box".into(), "allow sam/box".into()),
        (format!("pipeline {}", now() + 3600), "back".into()),
        ("back".into(), format!("pipeline {}", now() + 3600)),
    ]
}

#[test]
fn admission_retries_preserve_legacy_answers_and_publish_rollback_aliases() {
    for (command, newer) in cases() {
        for original in 0..4 {
            for retry in 0..4 {
                let dir = tempfile::tempdir().unwrap();
                let cfg = Config {
                    overlay: dir.path().join("overlay.json"),
                    ..Default::default()
                };
                let paths = paths(dir.path());
                let answer = legacy_request(&cfg, &paths[original], &command);
                cfg.command(&words(&newer), "host").unwrap();
                let before = cfg.readings().unwrap();
                assert_eq!(
                    cfg.request(&paths[retry], &words(&command), "upgrade")
                        .unwrap(),
                    answer
                );
                assert_eq!(
                    cfg.readings().unwrap(),
                    before,
                    "{command}: {original} -> {retry}"
                );
                // Both the retained old consumer and rollback using the new
                // spelling must recognize the answer with exact-key lookup.
                for path in [&paths[original], &paths[retry]] {
                    assert_eq!(legacy_request(&cfg, path, &command), answer);
                    assert_eq!(cfg.readings().unwrap(), before);
                }
            }
        }
    }
}

#[test]
fn new_admission_requests_remain_replay_safe_after_rollback_and_retained_writes() {
    for (command, newer) in cases() {
        for original in 0..4 {
            for retry in 0..4 {
                let dir = tempfile::tempdir().unwrap();
                let cfg = Config {
                    overlay: dir.path().join("overlay.json"),
                    ..Default::default()
                };
                let paths = paths(dir.path());
                let answer = cfg
                    .request(&paths[original], &words(&command), "new generation")
                    .unwrap();
                // An old writer can still change another cycle, retaining all
                // aliases in the unchanged admission document format.
                legacy_request(&cfg, &paths[original].with_file_name("newer-cycle"), &newer);
                let before = cfg.readings().unwrap();
                assert_eq!(legacy_request(&cfg, &paths[original], &command), answer);
                assert_eq!(cfg.readings().unwrap(), before);
                assert_eq!(
                    cfg.request(&paths[retry], &words(&command), "retry")
                        .unwrap(),
                    answer
                );
                assert_eq!(cfg.readings().unwrap(), before);
                assert_eq!(legacy_request(&cfg, &paths[retry], &command), answer);
                assert_eq!(cfg.readings().unwrap(), before);
            }
        }
    }
}
