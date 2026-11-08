//! Refresh deadlines read by burrow-events, under the sweep's shared lock.

use super::{Broke, Config, write};
use serde_json::{Map, Value};
use std::{fs, io};

impl Config {
    pub(super) fn book(&self, repo: &str, next: Option<i64>, clear: bool) -> Result<(), Broke> {
        let wake = self.state.join("wake.json");
        let broke = |e| Broke(format!("{}: {e}", wake.display()));
        let mut booked: Map<String, Value> = match fs::read(&wake) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| Broke(format!("{}: {e}", wake.display())))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Map::new(),
            Err(e) => return Err(broke(e)),
        };
        let key = repo.to_lowercase();
        if let Some(at) = next {
            booked.insert(key, Value::from(at));
        } else if clear {
            booked.remove(&key);
        }
        // Readers see a whole map; other writers hold sweep.lock.
        let tmp = self.state.join("wake.json.tmp");
        write(&tmp, &Value::Object(booked).to_string())?;
        fs::rename(&tmp, &wake).map_err(broke)
    }
}
