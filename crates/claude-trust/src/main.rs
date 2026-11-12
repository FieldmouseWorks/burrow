//! claude-trust DIR...: marks each DIR trusted in the user's Claude Code
//! config (~/.claude.json), the answer to the question Claude Code asks on
//! its first start in a folder, for the Claudes that start in one with
//! nobody at their terminal to answer it: the Remote Control servers
//! (modules/remote-control/serve.sh) and the usage watcher's Claude Code
//! (modules/usage-reset.nix). Claude keys trust by the folder's real path;
//! the spelling given is seeded too. A folder that isn't there yet is
//! trusted at the path it will have. Every argument is resolved before the
//! config is touched; one that can't be fails the run.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::thread::sleep;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value};

/// Tries at the lock, a tenth of a second apart.
const TRIES: u32 = 100;
const RETRY: Duration = Duration::from_millis(100);
/// Claude Code's own lock goes stale after 10 seconds; a crash's leftover gets 30.
/// Symlinks followed in one path before giving up, as the kernel does.
const MAX_LINKS: u32 = 40;
const STALE: Duration = Duration::from_secs(30);

fn main() -> ExitCode {
    let dirs: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    if dirs.is_empty() {
        eprintln!("usage: claude-trust DIR...");
        return ExitCode::from(2);
    }
    match run(&dirs) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(dirs: &[PathBuf]) -> Result<(), String> {
    let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
    let cfg = Path::new(&home).join(".claude.json");

    let mut paths: Vec<String> = Vec::new();
    for dir in dirs {
        for path in [dir.clone(), real_path(dir)?] {
            let path = path.to_string_lossy().into_owned();
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }

    let config = read(&cfg)?;
    if all_trusted(&config, &paths) {
        return Ok(());
    }

    let _lock = Lock::take(&cfg)?;
    // Read again under the lock: a running session may have written meanwhile.
    let mut config = read(&cfg)?;
    trust(&mut config, &paths)?;
    write(&cfg, &config).map_err(|e| format!("{}: {e}", cfg.display()))
}

fn read(cfg: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(cfg).unwrap_or_default();
    if text.trim().is_empty() {
        return Err(format!(
            "no {} yet: log in to Claude Code first (claude /login)",
            cfg.display()
        ));
    }
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", cfg.display()))
}

fn all_trusted(config: &Value, paths: &[String]) -> bool {
    paths
        .iter()
        .all(|p| config["projects"][p.as_str()]["hasTrustDialogAccepted"] == Value::Bool(true))
}

fn trust(config: &mut Value, paths: &[String]) -> Result<(), String> {
    let root = config
        .as_object_mut()
        .ok_or("the config is not a JSON object")?;
    let projects = root
        .entry("projects")
        .or_insert_with(|| Value::Object(Map::new()));
    if projects.is_null() {
        *projects = Value::Object(Map::new());
    }
    let projects = projects
        .as_object_mut()
        .ok_or("the config's projects is not an object")?;
    for path in paths {
        let project = projects
            .entry(path.as_str())
            .or_insert_with(|| Value::Object(Map::new()));
        if project.is_null() {
            *project = Value::Object(Map::new());
        }
        project
            .as_object_mut()
            .ok_or_else(|| format!("the config's project {path} is not an object"))?
            .insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    }
    Ok(())
}

/// Temp file in the config's folder, then a rename over it, mode 600.
fn write(cfg: &Path, config: &Value) -> io::Result<()> {
    let dir = cfg.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(&mut tmp, config)?;
    tmp.write_all(b"\n")?;
    tmp.as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    tmp.persist(cfg).map_err(|e| e.error)?;
    Ok(())
}

/// The lock Claude Code itself takes on its config (a `.lock` directory), so
/// this never interleaves with a running session's write. Released on drop.
struct Lock(PathBuf);

impl Lock {
    fn take(cfg: &Path) -> Result<Lock, String> {
        let mut name: OsString = cfg.as_os_str().to_owned();
        name.push(".lock");
        let path = PathBuf::from(name);
        for _ in 0..TRIES {
            if fs::create_dir(&path).is_ok() {
                return Ok(Lock(path));
            }
            if is_stale(&path) {
                let _ = fs::remove_dir(&path);
            }
            sleep(RETRY);
        }
        Err(format!(
            "{} stayed locked; trying again on the next start",
            cfg.display()
        ))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .is_some_and(|age| age > STALE)
}

/// `realpath -m`: every symlink followed (absolute, relative, chained,
/// dangling), `.` and `..` folded, the missing rest taken as written.
fn real_path(dir: &Path) -> Result<PathBuf, String> {
    if dir.as_os_str().is_empty() {
        return Err("empty path".into());
    }
    let absolute =
        std::path::absolute(dir).map_err(|e| format!("{}: cannot resolve: {e}", dir.display()))?;
    let mut todo: Vec<OsString> = absolute
        .components()
        .rev()
        .filter_map(component_name)
        .collect();
    let mut out = PathBuf::from("/");
    let mut links = 0;
    while let Some(name) = todo.pop() {
        if name == ".." {
            out.pop();
            continue;
        }
        if name == "." {
            continue;
        }
        let next = out.join(&name);
        match fs::read_link(&next) {
            Ok(target) => {
                links += 1;
                if links > MAX_LINKS {
                    return Err(format!(
                        "{}: too many levels of symbolic links",
                        dir.display()
                    ));
                }
                if target.is_absolute() {
                    out = PathBuf::from("/");
                }
                todo.extend(target.components().rev().filter_map(component_name));
            }
            Err(_) => out = next,
        }
    }
    Ok(out)
}

fn component_name(part: Component) -> Option<OsString> {
    match part {
        Component::Normal(name) => Some(name.to_owned()),
        Component::ParentDir => Some("..".into()),
        _ => None,
    }
}
