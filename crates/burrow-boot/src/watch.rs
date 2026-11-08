//! Work completion events retry queued trials; the daily timer is a backstop.
use crate::{Broke, Config};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

pub fn run(cfg: &Config) -> Result<u8, Broke> {
    let mut child = Command::new(&cfg.tools.journalctl)
        .args(["--follow", "--lines=0", "--quiet", "--output=json"])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| Broke(e.to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Broke("journalctl gave no stdout".into()))?;
    let (tx, rx) = mpsc::channel();
    let own = format!("{}-trial", cfg.boot);
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else {
                break;
            };
            if wakes(&line, &own) && tx.send(()).is_err() {
                break;
            }
        }
    });
    kick(cfg)?;
    while rx.recv().is_ok() {
        let until = Instant::now() + Duration::from_secs(2);
        while let Some(left) = until.checked_duration_since(Instant::now()) {
            if rx.recv_timeout(left).is_err() {
                break;
            }
        }
        kick(cfg)?;
    }
    let status = child.wait().map_err(|e| Broke(e.to_string()))?;
    Err(Broke(format!("journalctl stopped following: {status}")))
}
fn kick(cfg: &Config) -> Result<(), Broke> {
    if !cfg.state.join(crate::boot::TRIAL).exists() {
        return Ok(());
    }
    let status = Command::new(&cfg.tools.systemctl)
        .args([
            "start",
            "--no-block",
            &format!("{}-trial.service", cfg.boot),
        ])
        .status()
        .map_err(|e| Broke(e.to_string()))?;
    if status.success() {
        Ok(())
    } else {
        Err(Broke(format!("start trial: {status}")))
    }
}
fn wakes(line: &str, own: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return false;
    };
    let text = |key: &str| v.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
    if ["UNIT", "USER_UNIT", "_SYSTEMD_UNIT"]
        .iter()
        .any(|key| text(key).starts_with(own))
    {
        return false;
    }
    matches!(
        text("MESSAGE_ID"),
        "9d1aaa27d60140bd96365438aad20286"
            | "be02cf6855d2428ba40df7e9d022f03d"
            | "39f53479d3a045ac8e11786248231fbf"
    ) || text("MESSAGE").contains("completed with result:")
}
#[cfg(test)]
mod tests {
    use super::wakes;
    #[test]
    fn completion_wakes_without_a_self_trigger_loop() {
        assert!(wakes(
            r#"{"MESSAGE_ID":"9d1aaa27d60140bd96365438aad20286","USER_UNIT":"proj-1.service"}"#,
            "box-boot-trial"
        ));
        assert!(wakes(
            r#"{"MESSAGE":"Job completed with result: Succeeded"}"#,
            "box-boot-trial"
        ));
        assert!(!wakes(
            r#"{"MESSAGE_ID":"9d1aaa27d60140bd96365438aad20286","UNIT":"box-boot-trial.service"}"#,
            "box-boot-trial"
        ));
        assert!(!wakes(r#"{"MESSAGE":"unrelated"}"#, "box-boot-trial"));
    }
}
