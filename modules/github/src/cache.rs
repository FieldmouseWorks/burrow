//! Tokens already minted, each in its own file in a directory only this
//! tool's user can read, until it's within MARGIN of expiring: so a token
//! handed out works for at least that long, and GitHub is asked for a new
//! one only that often.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::app::Minted;

/// Seconds a token handed out still has to run, at least: fifty minutes, so
/// one taken at a job's start (Renovate's for a repo, which it never asks
/// again) lasts the job. GitHub's last an hour, so one is minted at most
/// every ten minutes for each target.
pub const MARGIN: i64 = 3000;

/// The token kept under `key`, unless it's within MARGIN of expiring.
pub fn get(dir: &Path, key: &str, now: i64) -> Option<String> {
    let text = fs::read_to_string(dir.join(key)).ok()?;
    let (expires, token) = text.trim_end().split_once(' ')?;
    (expires.parse::<i64>().ok()? - now > MARGIN && !token.is_empty()).then(|| token.to_owned())
}

/// Held while one asker mints `key`'s token, so the others wait for it and
/// take what it kept rather than each minting their own. Let go when it's
/// dropped, or when the process ends, however it ends.
pub fn lock(dir: &Path, key: &str) -> io::Result<File> {
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(dir.join(format!(".{key}.lock")))?;
    f.lock()?;
    Ok(f)
}

/// Keeps it under `key`, readable by this user only, whole or not at all.
pub fn put(dir: &Path, key: &str, minted: &Minted) -> io::Result<()> {
    let tmp = dir.join(format!(".{key}.{}", std::process::id()));
    // A crash's leftover under the same pid.
    let _ = fs::remove_file(&tmp);
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(format!("{} {}\n", minted.expires, minted.token).as_bytes())?;
    drop(f);
    fs::rename(&tmp, dir.join(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn keeps_a_token_until_its_margin() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(get(dir.path(), "sam", 0), None);
        let minted = Minted {
            token: "ghs_abc".into(),
            expires: 3600,
        };
        put(dir.path(), "sam", &minted).unwrap();
        assert_eq!(get(dir.path(), "sam", 0).as_deref(), Some("ghs_abc"));
        assert_eq!(
            get(dir.path(), "sam", 3600 - MARGIN - 1).as_deref(),
            Some("ghs_abc")
        );
        assert_eq!(get(dir.path(), "sam", 3600 - MARGIN), None);
        assert_eq!(get(dir.path(), "eve", 0), None);
        let mode = fs::metadata(dir.path().join("sam"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // Only the file itself: no leftover beside it.
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn keeps_github_s_tokens_as_they_come() {
        let dir = tempfile::tempdir().unwrap();
        let minted = Minted {
            token: crate::fake::TOKEN.into(),
            expires: 3600,
        };
        put(dir.path(), "sam", &minted).unwrap();
        assert_eq!(
            get(dir.path(), "sam", 0).as_deref(),
            Some(crate::fake::TOKEN)
        );
    }

    #[test]
    fn hands_out_only_a_token_with_fifty_minutes_to_run() {
        let dir = tempfile::tempdir().unwrap();
        let minted = Minted {
            token: "ghs_abc".into(),
            expires: 3600,
        };
        put(dir.path(), "sam", &minted).unwrap();
        assert!(get(dir.path(), "sam", 3600 - 50 * 60 - 1).is_some());
        assert_eq!(get(dir.path(), "sam", 3600 - 50 * 60), None);
    }

    #[test]
    fn a_garbled_file_is_no_token() {
        let dir = tempfile::tempdir().unwrap();
        for text in ["", "ghs_abc", "soon ghs_abc", "9999999999 "] {
            fs::write(dir.path().join("sam"), text).unwrap();
            assert_eq!(get(dir.path(), "sam", 0), None, "{text:?}");
        }
    }
}
