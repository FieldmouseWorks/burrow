//! A producer's secrets come only as systemd credentials (Conary's
//! `docs/m6-plugins.md`, "One key per producer"): a file in
//! `$CREDENTIALS_DIRECTORY`, named plainly, which no user but the
//! producer's (and root) may read. Never from the environment, an argument
//! or a path. The signer's key is one ([`crate::sign::Signer::load`]); a
//! judge's seal key, which it shares with the flow alone, is another.
use crate::sign::others_may_read;
use std::ffi::OsString;
use std::path::Path;

/// A file's POSIX access ACL, as the kernel keeps it in its extended
/// attributes.
const ACCESS_ACL: &str = "system.posix_acl_access";

/// The bytes of the systemd credential named `name`.
pub fn read(name: &str) -> Result<Vec<u8>, String> {
    read_from(&|var| std::env::var_os(var), name)
}

/// [`read`], reading the environment through `env`. The only variable it
/// reads is `CREDENTIALS_DIRECTORY`.
pub(crate) fn read_from(
    env: &dyn Fn(&str) -> Option<OsString>,
    name: &str,
) -> Result<Vec<u8>, String> {
    use std::os::unix::fs::MetadataExt;
    let plain = !name.is_empty() && name != "." && name != ".." && !name.contains('/');
    if !plain {
        return Err(format!(
            "{name:?} isn't a credential's name: a secret comes only as a systemd credential"
        ));
    }
    let dir = env("CREDENTIALS_DIRECTORY").ok_or(
        "no $CREDENTIALS_DIRECTORY: a producer's secret comes only as a systemd credential",
    )?;
    let dir = Path::new(&dir);
    if !dir.is_absolute() {
        return Err("$CREDENTIALS_DIRECTORY isn't an absolute path".into());
    }
    let path = dir.join(name);
    let meta = std::fs::symlink_metadata(&path).map_err(|e| format!("{name}: {e}"))?;
    if !meta.is_file() {
        return Err(format!("{name} isn't a file"));
    }
    let mut buf = vec![0u8; 4096];
    let acl = match rustix::fs::getxattr(&path, ACCESS_ACL, &mut buf[..]) {
        Ok(n) => buf.get(..n),
        Err(rustix::io::Errno::NODATA | rustix::io::Errno::NOTSUP) => None,
        Err(e) => return Err(format!("{name}'s ACL: {e}")),
    };
    if others_may_read(
        meta.mode(),
        meta.uid(),
        acl,
        rustix::process::geteuid().as_raw(),
    )? {
        return Err(format!(
            "{name} can be read by others than its owner, so it isn't this producer's alone"
        ));
    }
    std::fs::read(&path).map_err(|e| format!("{name}: {e}"))
}
