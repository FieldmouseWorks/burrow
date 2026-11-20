//! What a producer keeps (Conary's `docs/m6-plugins.md`, "Every request is
//! new work" and "Restart and cancellation"): one record per result, kept
//! durably and atomically before anything of it is published, so that a
//! crash between the work and its publication can be finished at startup
//! from the record, never by doing the work again.
//!
//! A record is never an answer. Nothing public reads one back: a request
//! always runs new work, and the only way back to a record is startup
//! reconciliation ([`crate::publish::reconcile`]).
//!
//! A record's life: [`Records::begin`] before its check run starts,
//! [`Records::keep`] once the work has its result, [`Records::commit`] once
//! something has gone out that publication must follow (a judgment's
//! sealed comment), and then one end, published or withdrawn, which
//! [`crate::publish`] alone writes. Until it's committed, its work being
//! abandoned withdraws it, and so does startup.
use crate::sign::Signable;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// A producer's records, one file each in one directory beside its work.
pub struct Records {
    dir: PathBuf,
}

/// What a result is published as: check run `check` on `repo`'s commit
/// `head`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub repo: String,
    pub head: String,
    pub check: String,
}

/// How a record ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum End {
    /// Its check run was completed with its signed result.
    Published,
    /// Its check run, if it had one, was completed as `cancelled`, and it
    /// will never be published.
    Withdrawn,
}

/// One result, as kept. Only [`Records`] makes one, and only this crate
/// reads one back.
#[derive(Debug, PartialEq, Eq)]
pub struct Record<T> {
    kept: Kept<T>,
}

/// A record as its file holds it. Private, so nothing outside the crate
/// can make a [`Record`] by reading one in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Kept<T> {
    /// New for every record: its check run's `external_id`, so a run a
    /// crash started can be found again.
    id: String,
    target: Target,
    /// Its check run, once started.
    run: Option<u64>,
    /// The work's result, once it has one.
    data: Option<T>,
    committed: bool,
    end: Option<End>,
}

impl<T> Record<T> {
    pub fn id(&self) -> &str {
        &self.kept.id
    }
    pub fn target(&self) -> &Target {
        &self.kept.target
    }
    /// The result it holds, once kept.
    pub fn data(&self) -> Option<&T> {
        self.kept.data.as_ref()
    }
    pub fn committed(&self) -> bool {
        self.kept.committed
    }
    pub fn end(&self) -> Option<End> {
        self.kept.end
    }
    pub(crate) fn run(&self) -> Option<u64> {
        self.kept.run
    }
}

impl Records {
    /// The records in `dir`, made (readable by this user alone) if it isn't
    /// there. One that's there must be this user's alone already.
    pub fn open(dir: &Path) -> io::Result<Self> {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} can be read by others than its owner", dir.display()),
            ));
        }
        Ok(Self { dir: dir.into() })
    }

    /// A new record, under a new id, for a result to be published as
    /// `target`: kept before its check run starts.
    pub fn begin<T: Serialize>(&self, target: Target) -> io::Result<Record<T>> {
        let record = Record {
            kept: Kept {
                id: new_id()?,
                target,
                run: None,
                data: None,
                committed: false,
                end: None,
            },
        };
        self.write(&record)?;
        Ok(record)
    }

    /// Keep the work's result in `record`, before anything of it is
    /// published. A record holds one result, for its own target.
    pub fn keep<T: Signable>(&self, record: &mut Record<T>, data: T) -> Result<(), String> {
        if record.kept.end.is_some() || record.kept.data.is_some() {
            return Err(format!("record {} already holds its result", record.id()));
        }
        if &data.target() != record.target() {
            return Err(format!(
                "record {} is for {:?}, not {:?}",
                record.id(),
                record.target(),
                data.target()
            ));
        }
        self.change(record, |k| k.data = Some(data))
    }

    /// Mark `record` past the point of no return: its work being abandoned
    /// no longer withdraws it, and startup publishes it.
    pub fn commit<T: Serialize + Clone>(&self, record: &mut Record<T>) -> Result<(), String> {
        if record.kept.end.is_some() || record.kept.data.is_none() {
            return Err(format!(
                "record {} has no result to commit, or has ended",
                record.id()
            ));
        }
        self.change(record, |k| k.committed = true)
    }

    pub(crate) fn started<T: Serialize + Clone>(
        &self,
        record: &mut Record<T>,
        run: u64,
    ) -> Result<(), String> {
        self.change(record, |k| k.run = Some(run))
    }

    pub(crate) fn ended<T: Serialize + Clone>(
        &self,
        record: &mut Record<T>,
        end: End,
    ) -> Result<(), String> {
        self.change(record, |k| k.end = Some(end))
    }

    /// Every record that hasn't ended, oldest id first, for startup to
    /// finish. One that doesn't read is an error, never left out.
    pub(crate) fn unfinished<T: DeserializeOwned>(&self) -> io::Result<Vec<Record<T>>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let kept: Kept<T> = serde_json::from_slice(&fs::read(&path)?).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?;
            if kept.end.is_none() {
                out.push(Record { kept });
            }
        }
        out.sort_by(|a, b| a.id().cmp(b.id()));
        Ok(out)
    }

    /// Change `record` on disk, and then in memory: if the write fails,
    /// it's left as the file still has it.
    fn change<T: Serialize + Clone>(
        &self,
        record: &mut Record<T>,
        how: impl FnOnce(&mut Kept<T>),
    ) -> Result<(), String> {
        let mut changed = Record {
            kept: record.kept.clone(),
        };
        how(&mut changed.kept);
        self.write(&changed)
            .map_err(|e| format!("record {}: {e}", record.id()))?;
        *record = changed;
        Ok(())
    }

    /// Written whole or not at all: a new file, synced, renamed over the
    /// old, and the directory synced.
    fn write<T: Serialize>(&self, record: &Record<T>) -> io::Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.dir)?;
        file.write_all(&serde_json::to_vec(&record.kept)?)?;
        file.as_file().sync_all()?;
        file.persist(self.dir.join(format!("{}.json", record.id())))
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
