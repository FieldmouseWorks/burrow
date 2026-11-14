//! What a producer keeps (Conary's `docs/m6-plugins.md`, "Every request is
//! new work"): each result, durably and atomically, before any of it is
//! published, so that a crash between the work and its publication can be
//! finished at startup from the record, never by doing the work again.
//!
//! A record is never an answer. Nothing here looks a record up by what was
//! asked: a request always runs new work, and the only way back to a record
//! is [`Records::unpublished`], which startup reconciliation reads.
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A producer's records, one file each in one directory beside its work.
pub struct Records {
    dir: PathBuf,
}

/// One result, as kept: its own id, what it holds, and whether its
/// publication finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record<T> {
    /// New for every record: a check run's `external_id`, so a run a crash
    /// started can be found again.
    pub id: String,
    pub data: T,
    published: bool,
}

impl<T> Record<T> {
    pub fn published(&self) -> bool {
        self.published
    }
}

impl Records {
    /// The records in `dir`, made (readable by this user alone) if it isn't
    /// there.
    pub fn open(dir: &Path) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        Ok(Self { dir: dir.into() })
    }

    /// Keep `data` as a new record, under a new id, before anything of it
    /// is published.
    pub fn keep<T: Serialize>(&self, data: T) -> io::Result<Record<T>> {
        let record = Record {
            id: new_id()?,
            data,
            published: false,
        };
        self.write(&record)?;
        Ok(record)
    }

    /// Keep what `record` holds now, in place of what it held.
    pub fn update<T: Serialize>(&self, record: &Record<T>) -> io::Result<()> {
        self.write(record)
    }

    /// Mark `record`'s publication finished: startup leaves it be.
    pub fn publish<T: Serialize>(&self, record: &mut Record<T>) -> io::Result<()> {
        record.published = true;
        self.write(record)
    }

    /// Every record whose publication didn't finish, for startup to finish.
    /// One that doesn't read is an error, never left out.
    pub fn unpublished<T: DeserializeOwned>(&self) -> io::Result<Vec<Record<T>>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let record: Record<T> = serde_json::from_slice(&fs::read(&path)?).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?;
            if !record.published {
                out.push(record);
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    /// Written whole or not at all: a new file, synced, renamed over the
    /// old, and the directory synced.
    fn write<T: Serialize>(&self, record: &Record<T>) -> io::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.dir)?;
        file.write_all(&serde_json::to_vec(record)?)?;
        file.as_file().sync_all()?;
        file.persist(self.dir.join(format!("{}.json", record.id)))
            .map_err(|e| e.error)?;
        fs::File::open(&self.dir)?.sync_all()
    }
}

/// 128 random bits, in hex.
fn new_id() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    let mut filled = 0;
    while filled < bytes.len() {
        filled +=
            rustix::rand::getrandom(&mut bytes[filled..], rustix::rand::GetRandomFlags::empty())?;
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
