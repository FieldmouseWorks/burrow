//! Where robot's credentials come from, with the file's reading stood in
//! for, so a file this user can't read is tested whoever runs the tests.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use burrow_robot::{Config, Env, credentials};

fn config() -> Config {
    Config {
        server_ip: "203.0.113.7".to_owned(),
        credentials_file: PathBuf::from("/run/secrets/robot"),
    }
}

/// A sudo that, asked to `cat FILE`, keeps its arguments in `sudo.log`,
/// says what `sudo.says` holds and exits with `sudo.exit`'s status, all
/// beside it. Written once for every test: a file being written while
/// another test starts a process is open for writing in that process until
/// it execs, and running the file then fails with "Text file busy".
fn sudo() -> &'static Path {
    static SUDO: OnceLock<(tempfile::TempDir, PathBuf)> = OnceLock::new();
    let (_, path) = SUDO.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sudo");
        fs::write(
            &path,
            "#!/bin/sh\necho \"$*\" >>\"$0.log\"\ncat \"$0.says\"\nexit \"$(cat \"$0.exit\")\"\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    });
    path
}

fn refused(_: &Path) -> io::Result<String> {
    Err(io::Error::from(io::ErrorKind::PermissionDenied))
}

fn never(_: &Path) -> io::Result<String> {
    panic!("the file was read")
}

#[test]
fn the_environment_wins_over_any_file() {
    let env = Env {
        user: Some("sam".to_owned()),
        password: Some("secret".to_owned()),
        creds_file: Some("/elsewhere".to_owned()),
        ..Env::default()
    };
    assert_eq!(
        credentials(&env, &config(), never, OsStr::new("/nonexistent/sudo")),
        Ok("sam:secret".to_owned())
    );
    let env = Env {
        user: Some("sam".to_owned()),
        ..Env::default()
    };
    assert_eq!(
        credentials(&env, &config(), never, OsStr::new("/nonexistent/sudo")),
        Ok("sam:".to_owned()),
        "no password"
    );
}

#[test]
fn the_file_is_robot_creds_file_or_the_configs() {
    let read = |p: &Path| Ok(format!("{}:pw\n\n", p.display()));
    assert_eq!(
        credentials(&Env::default(), &config(), read, OsStr::new("sudo")),
        Ok("/run/secrets/robot:pw".to_owned())
    );
    let env = Env {
        user: Some(String::new()),
        creds_file: Some("/elsewhere".to_owned()),
        ..Env::default()
    };
    assert_eq!(
        credentials(&env, &config(), read, OsStr::new("sudo")),
        Ok("/elsewhere:pw".to_owned())
    );
    let env = Env {
        creds_file: Some(String::new()),
        ..Env::default()
    };
    assert_eq!(
        credentials(&env, &config(), read, OsStr::new("sudo")),
        Ok("/run/secrets/robot:pw".to_owned()),
        "an empty ROBOT_CREDS_FILE is unset"
    );
}

#[test]
fn a_file_this_user_cant_read_is_read_through_sudo() {
    let sudo = sudo();
    let beside = |ext: &str| PathBuf::from(format!("{}.{ext}", sudo.display()));
    let run = |says: &str, exit: &str| {
        fs::write(beside("says"), says).unwrap();
        fs::write(beside("exit"), exit).unwrap();
        credentials(&Env::default(), &config(), refused, sudo.as_os_str())
    };
    assert_eq!(run("root:pw\n", "0"), Ok("root:pw".to_owned()));
    assert_eq!(
        fs::read_to_string(beside("log")).unwrap(),
        "cat /run/secrets/robot\n"
    );
    let failed = run("", "1").unwrap_err();
    assert!(
        failed.starts_with("sudo cat /run/secrets/robot failed ("),
        "{failed}"
    );
    let gone = credentials(
        &Env::default(),
        &config(),
        refused,
        OsStr::new("/nonexistent/sudo"),
    )
    .unwrap_err();
    assert!(gone.starts_with("sudo cat /run/secrets/robot: "), "{gone}");
}

#[test]
fn a_file_that_isnt_there_or_isnt_credentials_says_so() {
    let missing = |_: &Path| Err(io::Error::from(io::ErrorKind::NotFound));
    assert_eq!(
        credentials(&Env::default(), &config(), missing, OsStr::new("sudo")),
        Err("no Robot credentials: put user:password in /run/secrets/robot".to_owned())
    );
    let broken = |_: &Path| Err(io::Error::other("an I/O error"));
    assert_eq!(
        credentials(&Env::default(), &config(), broken, OsStr::new("sudo")),
        Err("/run/secrets/robot: an I/O error".to_owned())
    );
    let no_colon = |_: &Path| Ok("justauser\n".to_owned());
    assert_eq!(
        credentials(&Env::default(), &config(), no_colon, OsStr::new("sudo")),
        Err("/run/secrets/robot: not user:password".to_owned())
    );
}
