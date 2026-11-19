//! Fixtures shared by the workspace's tests.

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

/// Install an executable without ever opening it for writing in this process.
///
/// A concurrent fork can inherit a writer until it execs, making another
/// execution fail with ETXTBSY. Only the install child opens the destination;
/// the parent's temporary source is never executed. Wait for installation
/// before returning so callers can immediately run the result.
pub fn executable(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) {
    let path = path.as_ref();
    let mut source = tempfile::NamedTempFile::new().unwrap();
    source.write_all(contents.as_ref()).unwrap();
    let output = Command::new("install")
        .args(["-m", "0755", "--"])
        .arg(source.path())
        .arg(path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "install {}: {}",
        path.display(),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A command for a program under test that starts from the environment CI
/// gives it: every `PROJ_*` variable the test inherited is removed.
///
/// A proj job runs with its own `PROJ_JOB`, `PROJ_CONFIG`, sockets and the
/// rest, and proj's programs act on those first: a child that inherits them
/// talks to the job's host and config instead of the test's fixture. Spawn
/// proj's programs, and test binaries re-running themselves, through this, and
/// set the `PROJ_*` variables the test means on the returned command.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"PROJ_") {
            command.env_remove(key);
        }
    }
    command
}

/// Change fixture permissions, never making a regular file executable.
/// Directories need search bits; executable files must use [`executable`].
pub fn permissions(path: impl AsRef<Path>, mode: u32) {
    let path = path.as_ref();
    assert!(
        fs::metadata(path).unwrap().is_dir() || mode & 0o111 == 0,
        "use burrow_test::executable for {}",
        path.display()
    );
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Copy a data fixture, refusing executable sources or destinations.
/// `fs::copy` also copies execute bits, so executable copies must go through
/// [`executable`] with the source's bytes instead.
pub fn copy_file(from: impl AsRef<Path>, to: impl AsRef<Path>) {
    let (from, to) = (from.as_ref(), to.as_ref());
    for path in [from, to] {
        if let Ok(meta) = fs::metadata(path) {
            assert!(
                meta.permissions().mode() & 0o111 == 0,
                "use burrow_test::executable for {}",
                path.display()
            );
        }
    }
    fs::copy(from, to).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_install_and_execution() {
        std::thread::scope(|s| {
            for _ in 0..8 {
                s.spawn(|| {
                    let dir = tempfile::tempdir().unwrap();
                    let file = dir.path().join("stand-in");
                    for _ in 0..100 {
                        executable(&file, "#!/bin/sh\nprintf installed");
                        assert_eq!(
                            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                            0o755
                        );
                        let out = Command::new(&file).output().unwrap();
                        assert!(out.status.success());
                        assert_eq!(out.stdout, b"installed");
                    }
                });
            }
        });
    }

    #[test]
    #[should_panic(expected = "use burrow_test::executable")]
    fn permissions_cannot_make_a_file_executable() {
        let file = tempfile::NamedTempFile::new().unwrap();
        permissions(file.path(), 0o755);
    }
    #[test]
    #[should_panic(expected = "use burrow_test::executable")]
    fn copying_cannot_install_an_executable() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("executable");
        executable(&source, "#!/bin/sh\nexit 0\n");
        copy_file(source, dir.path().join("copy"));
    }

    /// The child the parent starts with a job's environment; it checks what a
    /// command built by [`command`] would see.
    #[test]
    fn command_child() {
        if std::env::var_os("PROJ_JOB").is_none() {
            return;
        }
        let out = command("env")
            .env("PROJ_CONFIG", "fixture")
            .output()
            .unwrap();
        let env = String::from_utf8(out.stdout).unwrap();
        let proj: Vec<_> = env.lines().filter(|l| l.starts_with("PROJ_")).collect();
        assert_eq!(proj, ["PROJ_CONFIG=fixture"]);
        println!("checked");
    }

    #[test]
    fn command_drops_the_inherited_job_environment() {
        let out = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::command_child", "--nocapture"])
            .env("PROJ_JOB", "job-1")
            .env("PROJ_CONFIG", "/job/config.json")
            .env("PROJ_HOST_JOBS_SOCKET", "/job/host.sock")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
        assert!(String::from_utf8_lossy(&out.stdout).contains("checked"));
    }
}
