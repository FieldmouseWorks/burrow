//! A write-ahead operation, recovered before any later CLI or door command.
//! The journal owns a resolved destination, never another interpretation of back.
use super::{Store, atomic_text, error, sync_parent};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize)]
struct Operation {
    receipt: PathBuf,
    destination: String,
    before: Option<String>,
    result: Option<String>,
}

impl Store<'_> {
    fn journal(&self) -> PathBuf {
        self.roles.with_file_name("roles.operation")
    }

    /// A request's receipt is immutable once completed, including after a
    /// subsequent CLI switch. Its caller supplies an identity for one open cycle.
    pub fn request(&self, receipt: &Path, args: &[String]) -> Result<String, String> {
        let _lock = self.lock()?;
        self.recover()?;
        if let Some(op) = read(receipt)? {
            return op
                .result
                .ok_or_else(|| error(receipt, "incomplete operation"));
        }
        let [command, name] = args else {
            return Err("expected use NAME".into());
        };
        if command != "use" {
            return Err("expected use NAME".into());
        }
        let destination = self.resolve(name)?;
        if !super::valid_name(&destination) || !self.names()?.contains(&destination) {
            return Err(format!("unknown setup: {destination}"));
        }
        self.checked(&self.dir.join(&destination))?;
        let receipt = fs::canonicalize(receipt.parent().ok_or("receipt has no parent")?)
            .map_err(|e| error(receipt, e))?
            .join(receipt.file_name().ok_or("receipt has no name")?);
        let op = Operation {
            receipt,
            destination,
            before: self.active()?,
            result: None,
        };
        // No roles or previous pointer may change until this rename and fsync
        // succeed. A receipt failure can therefore always recover this plan.
        write(&self.journal(), &op)?;
        self.finish(op)
    }

    /// Whether a failed request still has work to recover. Its door must leave
    /// the issue open instead of publishing a refusal for an uncertain switch.
    pub fn pending(&self) -> Result<bool, String> {
        self.journal()
            .try_exists()
            .map_err(|e| error(&self.journal(), e))
    }

    pub(super) fn recover(&self) -> Result<(), String> {
        if let Some(op) = read(&self.journal())? {
            self.finish(op)?;
        }
        Ok(())
    }

    fn finish(&self, mut op: Operation) -> Result<String, String> {
        if let Some(done) = read(&op.receipt)? {
            op.result = done.result;
        }
        if op.result.is_none() {
            // Repeating a resolved destination is idempotent. Every CLI command
            // recovers and durably records this result before making its own
            // change, so it cannot be mistaken for an unexecuted request later.
            self.switch(&op.destination, op.before.as_deref())?;
            op.result = Some(self.status(op.before.as_deref())?);
        }
        // Also sync a receipt left by an interrupted or failed prior fsync
        // before removing its recovery journal.
        write(&op.receipt, &op)?;
        fs::remove_file(self.journal()).map_err(|e| error(&self.journal(), e))?;
        sync_parent(&self.journal())?;
        op.result.ok_or_else(|| "operation has no result".into())
    }
}

fn read(path: &Path) -> Result<Option<Operation>, String> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| error(path, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(error(path, e)),
    }
}
fn write(path: &Path, op: &Operation) -> Result<(), String> {
    atomic_text(
        path,
        &serde_json::to_string(op).map_err(|e| error(path, e))?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn interruption_and_failed_receipt_cannot_replay_after_a_cli_switch() {
        for applied in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let roles = d.path().join("roles");
            fs::create_dir(d.path().join("roles.d")).unwrap();
            for (name, engine) in [
                ("one", "claude"),
                ("two", "codex"),
                ("three", "claude:chosen"),
            ] {
                fs::write(
                    d.path().join("roles.d").join(name),
                    format!("work {engine}\n"),
                )
                .unwrap();
            }
            let store = Store::new(&roles);
            store.command(&["use".into(), "one".into()]).unwrap();
            store.command(&["use".into(), "two".into()]).unwrap();
            let receipts = d.path().join("receipts");
            fs::create_dir(&receipts).unwrap();
            let receipt = receipts.join("result");
            let op = Operation {
                receipt: receipt.clone(),
                destination: "one".into(),
                before: Some("two".into()),
                result: None,
            };
            write(&store.journal(), &op).unwrap();
            if applied {
                store.switch("one", Some("two")).unwrap();
            }
            // Stop before or after the roles rename, then fail the result write.
            burrow_test::permissions(&receipts, 0o500);
            assert!(store.command(&["use".into(), "three".into()]).is_err());
            assert_eq!(store.active().unwrap().as_deref(), Some("one"));
            burrow_test::permissions(&receipts, 0o700);
            // A failed result rename is recoverable too; no switch can pass it.
            fs::create_dir(&receipt).unwrap();
            let done = Operation {
                result: Some("completed".into()),
                ..op
            };
            assert!(write(&receipt, &done).is_err());
            assert!(store.command(&["use".into(), "three".into()]).is_err());
            fs::remove_dir(&receipt).unwrap();
            store.command(&["use".into(), "three".into()]).unwrap();
            let said = store
                .request(&receipt, &["use".into(), "back".into()])
                .unwrap();
            assert!(said.contains("Was active: two\nActive: one"));
            assert_eq!(store.active().unwrap().as_deref(), Some("three"));
        }
    }

    #[test]
    fn a_journal_rename_failure_changes_nothing() {
        let d = tempfile::tempdir().unwrap();
        let roles = d.path().join("roles");
        fs::create_dir(d.path().join("roles.d")).unwrap();
        fs::write(&roles, "work claude\n").unwrap();
        fs::write(d.path().join("roles.d/two"), "work codex\n").unwrap();
        let store = Store::new(&roles);
        // An occupied directory refuses the journal's atomic rename.
        fs::create_dir(store.journal()).unwrap();
        assert!(
            store
                .request(&d.path().join("receipt"), &["use".into(), "two".into()])
                .is_err()
        );
        assert_eq!(fs::read_to_string(roles).unwrap(), "work claude\n");
    }
}
