//! An observer of every fresh reading; recording never changes reset decisions.

use crate::pass::{Account, Reading, Reset};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

/// The meter.json and meter.log wire format, shared with its consumers.
#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub utilization: f64,
    pub resets_at: String,
    pub read_at: f64,
}

pub struct Meter<A> {
    pub account: A,
    pub dir: PathBuf,
}
impl<A: Account> Account for Meter<A> {
    fn read(&mut self) -> Result<Reading, String> {
        let reading = self.account.read()?;
        if let Err(e) = record(&self.dir, &reading, crate::now()) {
            eprintln!("couldn't record meter: {e}");
        }
        Ok(reading)
    }
    fn reset(&mut self, keep: &mut dyn FnMut() -> Result<(), String>) -> Reset {
        self.account.reset(keep)
    }
}

/// The snapshot is atomic; history appends one JSON line for each reading,
/// including readings made to settle a reset. Failed reads append nothing.
pub fn record(dir: &Path, reading: &Reading, at: f64) -> Result<(), String> {
    let snapshot = Snapshot {
        utilization: reading.used,
        resets_at: reading.week.clone(),
        read_at: at,
    };
    let line = format!(
        "{}\n",
        serde_json::to_string(&snapshot).map_err(|e| e.to_string())?
    );
    crate::store::write(&dir.join("meter.json"), line.as_bytes())?;
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("meter.log"))
        .map_err(|e| e.to_string())?;
    log.write_all(line.as_bytes())
        .and_then(|()| log.sync_all())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn snapshot_is_last_reading_and_history_keeps_every_one() {
        let dir = std::env::temp_dir().join(format!("burrow-meter-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("meter.log"));
        let reading = Reading {
            used: 81.0,
            week: "2026-11-07T12:00:00Z".into(),
            ends: 20000.0,
        };
        record(&dir, &reading, 10000.0).unwrap();
        record(
            &dir,
            &Reading {
                used: 82.0,
                ..reading
            },
            10001.0,
        )
        .unwrap();
        let log = std::fs::read_to_string(dir.join("meter.log")).unwrap();
        let lines: Vec<_> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(
            std::fs::read_to_string(dir.join("meter.json"))
                .unwrap()
                .trim(),
            lines[1]
        );
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first["utilization"], 81.0);
        assert_eq!(first["read_at"], 10000.0);
        assert_eq!(first["resets_at"], "2026-11-07T12:00:00Z");
        assert!(!dir.join("meter.tmp").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
