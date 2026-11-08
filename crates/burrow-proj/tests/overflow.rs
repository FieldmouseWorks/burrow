//! Role overflow end to end: a config as the module writes it, the roles
//! file, the meters burrow-usage-reset keeps (one file an engine) and the
//! machine's usage hold, read the way a job's turn reads them. A role moves
//! to its overflow engine only when its own is short and the other has room,
//! and an engine's credits are room whatever its windows say.
use std::fs;
use std::path::Path;

use burrow_proj::config::Config;
use burrow_proj::engine::{Choice, Engine};
use serde_json::{Value, json};

/// The module's config, its jobs, roles and meters in DIR.
fn config(dir: &Path) -> Config {
    let path = |p: &str| dir.join(p).to_string_lossy().into_owned();
    serde_json::from_value(json!({
        "projects": path("src"),
        "jobs": path("jobs"),
        "scratch": path("scratch"),
        "slice": "claude-jobs.slice",
        "maxJobs": 4,
        "pipelineSlots": 1,
        "maxJobsPerProject": 2,
        "minFreeMemory": 4096,
        "jobMemoryMax": "8G",
        "timeout": "4h",
        "maxWait": 1800,
        "stallAfterMinutes": 120,
        "awaitHours": 24,
        "kick": "http://127.0.0.1:8788/kick",
        "wake": path("wake.json"),
        "waits": path("jobs/.waits.json"),
        "inboxMark": "<!-- proj-inbox -->",
        "pipelineRepos": ["sam/burrow"],
        "leadRepos": ["sam/burrow"],
        "flake": path("src/box"),
        "deployed": path("deployed"),
        "apps": {},
        "notify": "/bin/notify",
        "roles": path("roles"),
        "meters": path("meters"),
        "codexSlots": 2,
        "codexHooks": "/hooks",
        "inbox": null,
        "tools": {"codex": "/bin/codex", "bwrap": "/bin/bwrap", "bash": "/bin/bash", "flock": "/bin/flock", "gh": "/bin/gh", "git": "/bin/git",
                  "systemctl": "/bin/systemctl", "systemdRun": "/bin/systemd-run", "timeout": "/bin/timeout"}
    }))
    .unwrap()
}

/// ENGINE's meter in DIR, read now: each window's percent used, and its credits.
fn meter(dir: &Path, engine: &str, windows: &[(&str, f64)], credits: bool) {
    let windows: serde_json::Map<String, Value> = windows
        .iter()
        .map(|(n, used)| ((*n).to_owned(), json!({"used": used, "resets_at": 0})))
        .collect();
    let meters = dir.join("meters");
    fs::create_dir_all(&meters).unwrap();
    fs::write(
        meters.join(format!("{engine}.json")),
        json!({"windows": windows, "credits": credits, "read_at": burrow_proj::now()}).to_string(),
    )
    .unwrap();
}

fn on(engine: Engine, model: &str) -> Choice {
    Choice {
        engine,
        model: Some(model.into()),
    }
}

#[test]
fn claude_short_overflows_to_codex_while_codex_has_room_or_credits() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let cfg = config(dir);
    fs::create_dir_all(dir.join("jobs")).unwrap();
    fs::write(dir.join("roles"), "work claude:main over 90 codex:burst\n").unwrap();
    let stays = on(Engine::Claude, "main");
    let moves = on(Engine::Codex, "burst");

    // No meters yet: nothing to move it.
    assert_eq!(cfg.engine("work", None).unwrap(), stays);

    // Claude's week is fine but its session is nearly spent: any window counts.
    meter(dir, "codex", &[("300m", 10.0), ("10080m", 40.0)], false);
    meter(
        dir,
        "claude",
        &[("seven_day", 50.0), ("five_hour", 95.0)],
        false,
    );
    assert_eq!(cfg.engine("work", None).unwrap(), moves);
    meter(
        dir,
        "claude",
        &[("seven_day", 50.0), ("five_hour", 89.0)],
        false,
    );
    assert_eq!(cfg.engine("work", None).unwrap(), stays);

    // Codex's plan used up for the week: Claude keeps it, unless Codex has
    // credits to carry on with.
    meter(dir, "claude", &[("seven_day", 92.0)], false);
    meter(dir, "codex", &[("300m", 10.0), ("10080m", 100.0)], false);
    assert_eq!(cfg.engine("work", None).unwrap(), stays);
    meter(dir, "codex", &[("300m", 10.0), ("10080m", 100.0)], true);
    assert_eq!(cfg.engine("work", None).unwrap(), moves);

    // The machine out of Claude usage is short whatever its meter says.
    meter(dir, "claude", &[("seven_day", 10.0)], false);
    assert_eq!(cfg.engine("work", None).unwrap(), stays);
    let now = burrow_proj::now();
    fs::write(
        dir.join("jobs/.usage.json"),
        json!({"until": now + 3600, "said": "limit reached", "job": "j", "at": now}).to_string(),
    )
    .unwrap();
    assert_eq!(cfg.engine("work", None).unwrap(), moves);
}

#[test]
fn overflow_belongs_to_the_winning_scoped_line() {
    let d = tempfile::tempdir().unwrap();
    let dir = d.path();
    let cfg = config(dir);
    fs::write(
        dir.join("roles"),
        "work codex:bare\nwork@sam/code claude:scoped over 80 codex:burst\n",
    )
    .unwrap();
    meter(dir, "codex", &[("300m", 1.0)], false);
    for (used, expected) in [
        (79.0, on(Engine::Claude, "scoped")),
        (80.0, on(Engine::Codex, "burst")),
    ] {
        meter(dir, "claude", &[("seven_day", used)], false);
        assert_eq!(cfg.engine("work", Some("sam/code")).unwrap(), expected);
    }
}
