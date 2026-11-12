//! Legacy discovery is shared by read-only snapshots and locked first writes.
use crate::{Config, engines};
use std::fs::{self, File};

impl Config {
    /// Whether PATH is there: a link to nowhere is, and so is one that can't
    /// be looked at, which fails instead of passing for absent.
    pub(crate) fn present(path: &std::path::Path) -> Result<bool, String> {
        match path.symlink_metadata() {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub(crate) fn legacy_engines(&self) -> Result<Option<engines::Engines>, String> {
        let Some(dir) = &self.legacy else {
            return Ok(None);
        };
        if !Self::present(dir)? {
            return Ok(None);
        }
        if !fs::metadata(dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .is_dir()
        {
            return Err(format!(
                "{}: legacy configuration is not a directory",
                dir.display()
            ));
        }
        let names = [
            "roles",
            "models",
            "roles.d",
            "roles.previous",
            "roles.operation",
        ];
        let mut any = false;
        for n in names {
            any |= Self::present(&dir.join(n))?;
        }
        if !any {
            return Ok(None);
        }
        // A switch the old door left half done can't be adopted here.
        if Self::present(&dir.join("roles.operation"))? {
            return Err(format!(
                "{}: an unfinished engine switch; recover it with the previous generation",
                dir.join("roles.operation").display()
            ));
        }
        let text = |name: &str| match fs::read_to_string(dir.join(name)) {
            Ok(t) => Ok(t),
            // Absent means unset; a link to nowhere is a broken setting.
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound && !Self::present(&dir.join(name))? =>
            {
                Ok(String::new())
            }
            Err(e) => Err(format!("{}: {e}", dir.join(name).display())),
        };
        let mut setups = Vec::new();
        match fs::read_dir(dir.join("roles.d")) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|e| e.to_string())?;
                    if !entry.file_type().map_err(|e| e.to_string())?.is_dir() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let body = fs::read_to_string(entry.path())
                            .map_err(|e| format!("{}: {e}", entry.path().display()))?;
                        setups.push((name, body));
                    }
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    && !Self::present(&dir.join("roles.d"))? => {}
            Err(e) => return Err(format!("{}: {e}", dir.join("roles.d").display())),
        }
        let previous = text("roles.previous")?.trim().to_owned();
        let old = engines::import(
            &text("roles")?,
            &text("models")?,
            &setups,
            (!previous.is_empty()).then_some(previous),
        )
        .map_err(|e| format!("{}: {e}", dir.display()))?;
        old.validate()?;
        Ok(Some(old))
    }

    /// Share the earlier generation's lock while importing or adopting its result.
    pub(crate) fn legacy_lock(&self) -> Result<Option<File>, String> {
        let Some(dir) = &self.legacy else {
            return Ok(None);
        };
        if !Self::present(dir)? {
            return Ok(None);
        }
        let path = dir.join("roles.lock");
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        lock.lock()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Some(lock))
    }
}

/// An old request receipt is authoritative, including an unfinished one.
pub(crate) fn result(path: &std::path::Path) -> Result<Option<String>, String> {
    #[derive(serde::Deserialize)]
    struct Old {
        result: Option<String>,
    }
    match fs::read(path) {
        Ok(bytes) => {
            let old: Old =
                serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
            let body = old
                .result
                .ok_or_else(|| format!("{}: incomplete legacy operation", path.display()))?;
            crate::sync_file(path)?;
            Ok(Some(body))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !Config::present(path)? => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}
