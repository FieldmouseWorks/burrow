//! Saved roles files, checked with the turn's parser before an atomic switch.
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

mod operation;

pub struct Store<'a> {
    roles: &'a Path,
    dir: PathBuf,
    previous: PathBuf,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn error(path: &Path, e: impl std::fmt::Display) -> String {
    format!("{}: {e}", path.display())
}

impl<'a> Store<'a> {
    pub fn new(roles: &'a Path) -> Self {
        let parent = roles.parent().unwrap_or(Path::new("."));
        Self {
            roles,
            dir: parent.join("roles.d"),
            previous: parent.join("roles.previous"),
        }
    }

    fn names(&self) -> Result<Vec<String>, String> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(error(&self.dir, e)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| error(&self.dir, e))?;
            if entry
                .file_type()
                .map_err(|e| error(&entry.path(), e))?
                .is_file()
            {
                let name = entry.file_name().to_string_lossy().into_owned();
                if valid_name(&name) {
                    names.push(name);
                }
            }
        }
        names.sort();
        Ok(names)
    }

    fn active(&self) -> Result<Option<String>, String> {
        let current = match fs::read(self.roles) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(error(self.roles, e)),
        };
        // Prefer the link's own name when two setups have identical text.
        let target = fs::canonicalize(self.roles).ok();
        let names = self.names()?;
        for name in &names {
            if target.is_some() && fs::canonicalize(self.dir.join(name)).ok() == target {
                return Ok(Some(name.clone()));
            }
        }
        for name in names {
            let path = self.dir.join(&name);
            if fs::read(&path).map_err(|e| error(&path, e))? == current {
                return Ok(Some(name));
            }
        }
        Ok(None)
    }

    /// The saved setups and the one in force, as `settings show` gives
    /// them, each setup's roles checked as a turn checks them; None when
    /// none is saved.
    pub(crate) fn shown(&self) -> Result<Option<serde_json::Value>, String> {
        let names = self.names()?;
        if names.is_empty() {
            return Ok(None);
        }
        let mut saved = serde_json::Map::new();
        for name in names {
            let path = self.dir.join(&name);
            let text = fs::read_to_string(&path).map_err(|e| error(&path, e))?;
            saved.insert(name, crate::roles::shown(&path, &text)?.into());
        }
        Ok(Some(
            serde_json::json!({"active": self.active()?, "saved": saved}),
        ))
    }

    /// Where the saved setups are kept.
    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    fn previous(&self) -> Result<Option<String>, String> {
        match fs::read_to_string(&self.previous) {
            Ok(s) if valid_name(s.trim()) => Ok(Some(s.trim().into())),
            Ok(_) => Err(error(&self.previous, "invalid setup name")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(error(&self.previous, e)),
        }
    }

    fn checked(&self, path: &Path) -> Result<String, String> {
        let text = fs::read_to_string(path).map_err(|e| error(path, e))?;
        crate::roles::parse(path, &text, None, false, crate::now())?;
        Ok(text)
    }

    /// Serialized across the CLI and door, including the status they report.
    pub fn command(&self, args: &[String]) -> Result<String, String> {
        let _lock = self.lock()?;
        self.recover()?;
        let before = self.active()?;
        match args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            [] | ["list"] => {}
            ["save", name] => self.save(name, false)?,
            ["save", name, "--replace"] => self.save(name, true)?,
            ["use", name] => self.switch(&self.resolve(name)?, before.as_deref())?,
            _ => {
                return Err(
                    "usage: proj engines [save NAME [--replace] | use NAME | use back]".into(),
                );
            }
        }
        self.status(before.as_deref())
    }

    fn lock(&self) -> Result<File, String> {
        let parent = self.roles.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent).map_err(|e| error(parent, e))?;
        let lock_path = parent.join("roles.lock");
        let lock = File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .map_err(|e| error(&lock_path, e))?;
        lock.lock().map_err(|e| error(&lock_path, e))?;
        Ok(lock)
    }

    fn save(&self, name: &str, replace: bool) -> Result<(), String> {
        if !valid_name(name) || name == "back" {
            return Err("setup name must be [a-z0-9-]+ and not back".into());
        }
        let text = self.checked(self.roles)?;
        fs::create_dir_all(&self.dir).map_err(|e| error(&self.dir, e))?;
        let path = self.dir.join(name);
        if replace {
            atomic_text(&path, &text)
        } else {
            let mut f = File::options()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| error(&path, e))?;
            f.write_all(text.as_bytes())
                .and_then(|()| f.sync_all())
                .map_err(|e| error(&path, e))?;
            sync_parent(&path)
        }
    }

    fn resolve(&self, name: &str) -> Result<String, String> {
        if name == "back" {
            self.previous()?.ok_or("no previous setup".into())
        } else {
            Ok(name.to_owned())
        }
    }

    /// Only a resolved setup name, never another interpretation of back.
    fn switch(&self, name: &str, before: Option<&str>) -> Result<(), String> {
        let name = name.to_owned();
        // Membership before joining: issue text cannot reach another path.
        if !valid_name(&name) || !self.names()?.contains(&name) {
            return Err(format!("unknown setup: {name}"));
        }
        let target = self.dir.join(&name);
        self.checked(&target)?;
        let target = fs::canonicalize(&target).map_err(|e| error(&target, e))?;
        if self.roles.is_symlink() && fs::canonicalize(self.roles).ok().as_ref() == Some(&target) {
            return sync_parent(self.roles);
        }
        let mut old = before.map(str::to_owned);
        if old.is_none() && self.roles.is_file() {
            // Preserve even an invalid old file: a checked setup can repair it.
            let text = fs::read(self.roles).map_err(|e| error(self.roles, e))?;
            let stamp = jiff::Timestamp::now()
                .strftime("%Y%m%dT%H%M%S%fZ")
                .to_string()
                .to_ascii_lowercase();
            let name = format!("unsaved-{stamp}");
            let path = self.dir.join(&name);
            let mut f = File::options()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| error(&path, e))?;
            f.write_all(&text)
                .and_then(|()| f.sync_all())
                .map_err(|e| error(&path, e))?;
            sync_parent(&path)?;
            old = Some(name);
        }
        let staging = tempfile::tempdir_in(self.roles.parent().unwrap_or(Path::new(".")))
            .map_err(|e| error(self.roles, e))?;
        let tmp = staging.path().join("roles");
        // An absolute target works with either an absolute or relative roles path.
        symlink(&target, &tmp).map_err(|e| error(&tmp, e))?;
        let result = (|| {
            if let Some(old) = old {
                atomic_text(&self.previous, &old)?;
            }
            fs::rename(&tmp, self.roles).map_err(|e| error(self.roles, e))?;
            sync_parent(self.roles)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }

    fn status(&self, before: Option<&str>) -> Result<String, String> {
        let active = self.active()?;
        let previous = self.previous()?;
        let mut out = format!(
            "Was active: {}\nActive: {}\nPrevious: {}\n",
            before.unwrap_or("unsaved"),
            active.as_deref().unwrap_or("unsaved"),
            previous.as_deref().unwrap_or("none")
        );
        for name in self.names()? {
            out += &format!(
                "{} {name}\n",
                if active.as_deref() == Some(&name) {
                    "*"
                } else {
                    " "
                }
            );
        }
        let text = crate::roles::read(self.roles)?;
        out += &crate::roles::engines(self.roles, &text)?;
        Ok(out)
    }
}

fn sync_parent(path: &Path) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    File::open(parent)
        .and_then(|f| f.sync_all())
        .map_err(|e| error(parent, e))
}

fn atomic_text(path: &Path, text: &str) -> Result<(), String> {
    let parent = path.parent().unwrap_or(Path::new("."));
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| error(parent, e))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.as_file().sync_all())
        .map_err(|e| error(path, e))?;
    file.persist(path).map_err(|e| error(path, e))?;
    sync_parent(path)
}

#[cfg(test)]
mod tests;
