//! One socket-activated host operation: acknowledge crashes and request a pass.
//! Paths are map keys, never opened or passed to a command. Configuration,
//! timestamps and the lock come exclusively from the host service.
use super::Config;
use std::{
    io::{Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

const LIMIT: u64 = 65536;

pub(super) fn validate(exes: &[String]) -> Result<(), String> {
    if exes.is_empty()
        || exes.len() > 128
        || exes.iter().any(|exe| {
            !Path::new(exe).is_absolute() || exe.len() > 4096 || exe.chars().any(char::is_control)
        })
    {
        return Err("clear takes 1 to 128 absolute program paths, as the watch names them".into());
    }
    Ok(())
}

fn acknowledge(cfg: &Config, input: impl Read) -> Result<(), String> {
    let mut bytes = Vec::new();
    input
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("clear request is too large".into());
    }
    let exes: Vec<String> = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    validate(&exes)?;
    super::clear(cfg, &exes)?;
    super::request_pass(cfg)
}

pub(super) fn serve(cfg: &Config) -> Result<(), String> {
    let result = acknowledge(cfg, std::io::stdin().lock());
    serde_json::to_writer(std::io::stdout().lock(), &result).map_err(|e| e.to_string())
}

pub(super) fn client(path: &Path, exes: &[String]) -> Result<(), String> {
    let bytes = serde_json::to_vec(exes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > LIMIT {
        return Err("clear request is too large".into());
    }
    let mut stream = UnixStream::connect(path).map_err(|e| format!("{}: {e}", path.display()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(40)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream.write_all(&bytes).map_err(|e| e.to_string())?;
    stream
        .shutdown(Shutdown::Write)
        .map_err(|e| e.to_string())?;
    let response: Result<(), String> = serde_json::from_reader(stream.take(LIMIT))
        .map_err(|e| format!("host clear response: {e}"))?;
    response?;
    println!("cleared crashes so far and asked the host for a pass");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bounded_lists_of_absolute_program_names_are_accepted() {
        for request in [
            "[]",
            "[\"relative\"]",
            "[\"/bin/x\\n\"]",
            "{\"command\":\"pass\"}",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let cfg = config(dir.path());
            assert!(acknowledge(&cfg, request.as_bytes()).is_err());
            assert!(!dir.path().join("cleared.json").exists());
        }
        assert!(validate(&["/bin/a path; $(never executed)".into()]).is_ok());
        assert!(validate(&vec!["/bin/x".into(); 129]).is_err());
    }

    fn config(root: &Path) -> Config {
        serde_json::from_value(serde_json::json!({
            "file":root.join("watch.json"), "ignore":[], "grace":600,
            "window":86400, "systemctl":"false", "coredumpctl":"false",
            "journalctl":"false", "own":"http://127.0.0.1:1/units", "kick":null,
            "clearSocket":root.join("clear.sock")
        }))
        .unwrap()
    }
}
