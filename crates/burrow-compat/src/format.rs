use crate::{AdapterError, Checked};
use std::fmt;

type Read<V> = Box<dyn Fn(&[u8]) -> Result<V, AdapterError>>;
type Write<V> = Box<dyn Fn(&V) -> Result<Vec<u8>, AdapterError>>;
type Step<V> = Box<dyn Fn(V) -> Result<V, AdapterError>>;

/// Which way a record travels between writer and reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// A reader of the same version.
    Same,
    /// Old data, new reader: the upgrade.
    Forward,
    /// New data, old reader: the rollback.
    Backward,
}

/// Where in the trip it failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Write,
    Read,
    Migrate,
    /// Both succeeded but the reader saw something else than the migration says.
    Compare,
}

/// One failed trip of one value between two versions of one format.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Failure {
    pub format: String,
    pub direction: Direction,
    pub writer: u32,
    pub reader: u32,
    pub stage: Stage,
    pub field: Option<String>,
    pub message: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let way = match self.direction {
            Direction::Same => "same-version",
            Direction::Forward => "forward (old data, new reader)",
            Direction::Backward => "backward (new data, old reader)",
        };
        let stage = match self.stage {
            Stage::Write => "write",
            Stage::Read => "read",
            Stage::Migrate => "migration",
            Stage::Compare => "comparison",
        };
        write!(
            f,
            "format `{}` {way}: v{} written, v{} read, {stage} failed",
            self.format, self.writer, self.reader
        )?;
        if let Some(field) = &self.field {
            write!(f, " at field `{field}`")?;
        }
        write!(f, ": {}", self.message)
    }
}

struct Version<V> {
    number: u32,
    read: Read<V>,
    write: Write<V>,
    samples: Vec<V>,
}

struct Migration<V> {
    from: u32,
    up: Step<V>,
    down: Step<V>,
}

/// A persisted format: stable id, explicit versions in order, the steps
/// between neighbours. `V` is the format's value in memory; each version's
/// adapters are the production reader and writer of that version's bytes.
pub struct Format<V> {
    id: String,
    versions: Vec<Version<V>>,
    migrations: Vec<Migration<V>>,
}

impl<V: Clone + PartialEq + fmt::Debug> Format<V> {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            versions: Vec::new(),
            migrations: Vec::new(),
        }
    }

    /// Add the next version with its adapters and the values it can represent.
    /// Numbers must rise; a gap is a version nobody can read.
    pub fn version(
        mut self,
        number: u32,
        read: impl Fn(&[u8]) -> Result<V, AdapterError> + 'static,
        write: impl Fn(&V) -> Result<Vec<u8>, AdapterError> + 'static,
        samples: Vec<V>,
    ) -> Self {
        assert!(
            self.versions.last().is_none_or(|v| v.number < number),
            "format `{}`: version {number} does not follow {}",
            self.id,
            self.versions.last().map_or(0, |v| v.number)
        );
        self.versions.push(Version {
            number,
            read: Box::new(read),
            write: Box::new(write),
            samples,
        });
        self
    }

    /// The explicit migration from version `from` to the next one: `up` to
    /// carry a value forward, `down` to carry it back for a rollback.
    pub fn migrate(
        mut self,
        from: u32,
        up: impl Fn(V) -> Result<V, AdapterError> + 'static,
        down: impl Fn(V) -> Result<V, AdapterError> + 'static,
    ) -> Self {
        self.migrations.push(Migration {
            from,
            up: Box::new(up),
            down: Box::new(down),
        });
        self
    }

    fn carry(&self, mut value: V, from: usize, to: usize) -> Result<V, (u32, AdapterError)> {
        let step = |i: usize| {
            let at = self.versions[i].number;
            self.migrations
                .iter()
                .find(|m| m.from == at)
                .ok_or_else(|| {
                    (
                        at,
                        AdapterError::new(format!("no migration registered from v{at}")),
                    )
                })
        };
        if from <= to {
            for i in from..to {
                value = (step(i)?.up)(value).map_err(|e| (self.versions[i].number, e))?;
            }
        } else {
            for i in (to..from).rev() {
                value = (step(i)?.down)(value).map_err(|e| (self.versions[i].number, e))?;
            }
        }
        Ok(value)
    }

    /// Every failure of every trip: each version's values, written by it,
    /// read by every version (itself included), compared with the value
    /// carried there by the migrations.
    pub fn check(&self) -> Vec<Failure> {
        let mut out = Vec::new();
        for (w, writer) in self.versions.iter().enumerate() {
            for sample in &writer.samples {
                for (r, reader) in self.versions.iter().enumerate() {
                    let fail = |stage, e: AdapterError| Failure {
                        format: self.id.clone(),
                        direction: match w.cmp(&r) {
                            std::cmp::Ordering::Equal => Direction::Same,
                            std::cmp::Ordering::Less => Direction::Forward,
                            std::cmp::Ordering::Greater => Direction::Backward,
                        },
                        writer: writer.number,
                        reader: reader.number,
                        stage,
                        field: e.field,
                        message: e.message,
                    };
                    let bytes = match (writer.write)(sample) {
                        Ok(b) => b,
                        Err(e) => {
                            out.push(fail(Stage::Write, e));
                            continue;
                        }
                    };
                    let expected = match self.carry(sample.clone(), w, r) {
                        Ok(v) => v,
                        Err((at, e)) => {
                            let mut f = fail(Stage::Migrate, e);
                            f.message = format!("at v{at}: {}", f.message);
                            out.push(f);
                            continue;
                        }
                    };
                    match (reader.read)(&bytes) {
                        Err(e) => out.push(fail(Stage::Read, e)),
                        Ok(got) if got != expected => out.push(fail(
                            Stage::Compare,
                            AdapterError::new(format!(
                                "read {got:?}, migration gives {expected:?}"
                            )),
                        )),
                        Ok(_) => {}
                    }
                }
            }
        }
        out
    }

    /// Panic with every failure, one a line. For a crate's own tests.
    pub fn assert_compatible(&self) {
        let failures = self.check();
        assert!(
            failures.is_empty(),
            "{}",
            failures
                .iter()
                .map(Failure::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

impl<V: Clone + PartialEq + fmt::Debug> Checked for Format<V> {
    fn id(&self) -> &str {
        &self.id
    }

    fn check(&self) -> Vec<Failure> {
        Format::check(self)
    }
}
