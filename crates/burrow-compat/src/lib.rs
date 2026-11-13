//! A harness for formats burrow persists. A format has a stable id and
//! explicit versions; each version brings the production reader and writer
//! (bytes in, bytes out, whatever the encoding) and representative values.
//! `Format::check` writes every version's values with its own writer and
//! reads them with every other version's reader, forward (old data, new
//! reader) and backward (new data, old reader, the rollback case), and
//! compares with what the explicit migrations say the other version should
//! see. Every failure names the format, the direction, the versions and,
//! when the reader said which, the field. The owning guide is
//! docs/compatibility.md.
mod adapter;
mod format;
mod registry;
mod unversioned;

pub use adapter::AdapterError;
pub use format::{Direction, Failure, Format, Stage};
pub use registry::{Checked, Registry};
pub use unversioned::declared_version;
