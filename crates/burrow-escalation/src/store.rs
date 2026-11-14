//! Where the escalations are kept: one JSON file each, under a lock, each
//! write made whole on disk before it counts.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::PathBuf;

use crate::{Error, Record, check_key};

/// The lock on the records, held while it lives.
pub struct Held(#[allow(dead_code)] Option<File>);

impl Held {
    /// A lock that holds nothing, for records only one caller has.
    pub fn none() -> Held {
        Held(None)
    }
}

/// The escalations kept.
pub trait Records {
    /// Holds off every other caller until the returned lock goes.
    fn lock(&mut self) -> Result<Held, Error>;
    fn load(&mut self, key: &str) -> Result<Option<Record>, Error>;
    /// Kept for good once it returns: a crash after it can't undo it.
    fn save(&mut self, record: &Record) -> Result<(), Error>;
    /// Every key kept, in order.
    fn keys(&mut self) -> Result<Vec<String>, Error>;
}

/// The records in a directory: KEY.json each, and `.lock`.
pub struct Dir(pub PathBuf);

const SUFFIX: &str = ".json";

impl Dir {
    fn path(&self, key: &str) -> Result<PathBuf, Error> {
        check_key(key)?;
        Ok(self.0.join(format!("{key}{SUFFIX}")))
    }
}

fn said(what: impl std::fmt::Display, e: impl std::fmt::Display) -> Error {
    format!("{what}: {e}")
}

impl Records for Dir {
    fn lock(&mut self) -> Result<Held, Error> {
        fs::create_dir_all(&self.0).map_err(|e| said(self.0.display(), e))?;
        let path = self.0.join(".lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| said(path.display(), e))?;
        file.lock().map_err(|e| said(path.display(), e))?;
        Ok(Held(Some(file)))
    }

    fn load(&mut self, key: &str) -> Result<Option<Record>, Error> {
        let path = self.path(key)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(said(path.display(), e)),
        };
        let record: Record = serde_json::from_str(&text).map_err(|e| said(path.display(), e))?;
        if record.key != key {
            return Err(format!("{}: holds {:?}", path.display(), record.key));
        }
        Ok(Some(record))
    }

    fn save(&mut self, record: &Record) -> Result<(), Error> {
        let path = self.path(&record.key)?;
        let tmp = self.0.join(format!(".{}{SUFFIX}.tmp", record.key));
        let text = serde_json::to_string_pretty(record).map_err(|e| said(&record.key, e))?;
        let mut file = File::create(&tmp).map_err(|e| said(tmp.display(), e))?;
        file.write_all(format!("{text}\n").as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| said(tmp.display(), e))?;
        fs::rename(&tmp, &path).map_err(|e| said(path.display(), e))?;
        File::open(&self.0)
            .and_then(|d| d.sync_all())
            .map_err(|e| said(self.0.display(), e))
    }

    fn keys(&mut self) -> Result<Vec<String>, Error> {
        let entries = match fs::read_dir(&self.0) {
            Ok(entries) => entries,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(said(self.0.display(), e)),
        };
        let mut keys = Vec::new();
        for entry in entries {
            let name = entry.map_err(|e| said(self.0.display(), e))?.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(key) = name.strip_suffix(SUFFIX)
                && check_key(key).is_ok()
            {
                keys.push(key.to_owned());
            }
        }
        keys.sort();
        Ok(keys)
    }
}
