//! burrow-rustc-wrapper as RUSTC_WRAPPER's launcher runs it, with stand-ins
//! for sccache, the compiler and id that keep what they were run with.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// What each stand-in keeps: how it was run ($0 and its words) and the
/// SCCACHE_ variables it got, one line each, in $LOGS/NAME.
const KEEPS: &str = r#"name=${0##*/}
{ echo "$0 $*"; env | grep '^SCCACHE_' | sort; } >"$LOGS/$name"
"#;

/// The stand-ins, written once for every test: each keeps what it was run
/// with. A file being written while another test starts a process is open
/// for writing in that process until it execs, and running the file then
/// fails with "Text file busy". No test starts one before these are written,
/// and none is written after.
fn stand_ins() -> &'static Path {
    static BIN: OnceLock<tempfile::TempDir> = OnceLock::new();
    BIN.get_or_init(|| {
        let bin = tempfile::tempdir().unwrap();
        let b = bin.path();
        // sccache exits as SCCACHE_STAND_IN says, which a test can't put in
        // its environment, since the wrapper takes every SCCACHE_ variable
        // out: STAND_IN, or killed when that's `kill`.
        script(
            &b.join("sccache"),
            &format!(
                "#!/bin/sh\n{KEEPS}[ \"$STAND_IN\" = kill ] && kill -9 $$\nexit \"${{STAND_IN:-0}}\"\n"
            ),
        );
        script(
            &b.join("cc"),
            &format!("#!/bin/sh\n{KEEPS}exit \"${{CC_EXIT:-0}}\"\n"),
        );
        script(
            &b.join("id"),
            "#!/bin/sh\n[ \"$1\" = -un ] && [ -n \"$WHO\" ] && echo \"$WHO\"\n",
        );
        fs::write(b.join("not-runnable"), "#!/bin/sh\n").unwrap();
        bin
    })
    .path()
}

/// A machine's cache directory, with sam's cache and daemon in it.
struct Machine {
    dir: tempfile::TempDir,
    /// sam's daemon, listening for as long as the machine is around.
    _daemon: Option<UnixListener>,
}

impl Machine {
    fn new() -> Machine {
        Machine::with(true)
    }

    fn with(daemon: bool) -> Machine {
        // Before anything here can start a process.
        let bin = stand_ins();
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        fs::create_dir_all(d.join("root/sam/cache")).unwrap();
        fs::create_dir(d.join("logs")).unwrap();
        let _daemon = daemon.then(|| UnixListener::bind(d.join("root/sam/sccache.sock")).unwrap());
        let config = serde_json::json!({
            "root": d.join("root"),
            "conf": "/the/conf",
            "sccache": bin.join("sccache"),
            "id": bin.join("id"),
        });
        fs::write(d.join("config.json"), config.to_string()).unwrap();
        Machine { dir, _daemon }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// `burrow-rustc-wrapper CONFIG cc -c a.c` as sam, with `env` too.
    fn wrap(&self, env: &[(&str, &str)]) -> Output {
        self.wrap_as(Some("sam"), env)
    }

    /// The same with USER set to `user`, or unset.
    fn wrap_as(&self, user: Option<&str>, env: &[(&str, &str)]) -> Output {
        let config = self.path("config.json");
        self.run(&[config.as_os_str()], user, env)
    }

    fn run(&self, words: &[&OsStr], user: Option<&str>, env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_burrow-rustc-wrapper"));
        cmd.args(words)
            .arg(stand_ins().join("cc"))
            .args(["-c", "a.c"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("LOGS", self.path("logs"))
            .envs(env.iter().copied());
        if let Some(user) = user {
            cmd.env("USER", user);
        }
        cmd.output().unwrap()
    }

    /// What the stand-in `name` was run with, if it was.
    fn ran(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.path("logs").join(name)).ok()
    }

    fn sock(&self) -> String {
        self.path("root/sam/sccache.sock").display().to_string()
    }
}

fn code(out: &Output) -> Option<i32> {
    out.status.code()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn a_compile_goes_through_sccache_with_only_the_modules_settings() {
    let m = Machine::new();
    let out = m.wrap(&[("SCCACHE_LOG", "debug"), ("SCCACHE_DIR", "/elsewhere")]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert_eq!(stderr(&out), "");
    let cc = stand_ins().join("cc");
    assert_eq!(
        m.ran("sccache").unwrap(),
        format!(
            "{} {} -c a.c\nSCCACHE_CLIENT_SIDE=1\nSCCACHE_CONF=/the/conf\nSCCACHE_SERVER_UDS={}\n",
            stand_ins().join("sccache").display(),
            cc.display(),
            m.sock()
        )
    );
    // sccache compiles; nothing runs it again.
    assert_eq!(m.ran("cc"), None);
}

#[test]
fn the_compiles_exit_is_the_wrappers() {
    for exit in ["1", "101"] {
        let m = Machine::new();
        let out = m.wrap(&[("STAND_IN", exit)]);
        assert_eq!(code(&out), exit.parse().ok(), "{exit}");
        assert_eq!(m.ran("cc"), None, "{exit}");
    }
    // Killed, as a shell says it.
    let m = Machine::new();
    let out = m.wrap(&[("STAND_IN", "kill")]);
    assert_eq!(code(&out), Some(137));
    assert_eq!(m.ran("cc"), None);
}

#[test]
fn sccaches_own_failure_compiles_without_it() {
    let m = Machine::new();
    let out = m.wrap(&[
        ("STAND_IN", "2"),
        ("CC_EXIT", "3"),
        ("SCCACHE_LOG", "debug"),
    ]);
    assert_eq!(code(&out), Some(3), "{}", stderr(&out));
    assert!(m.ran("sccache").is_some());
    // The compile, as it came, with no SCCACHE_ variable left.
    assert_eq!(
        m.ran("cc").unwrap(),
        format!("{} -c a.c\n", stand_ins().join("cc").display())
    );
}

#[test]
fn an_sccache_that_cant_run_compiles_without_it() {
    let m = Machine::new();
    let mut config: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(m.path("config.json")).unwrap()).unwrap();
    config["sccache"] = "/nonexistent/sccache".into();
    fs::write(m.path("config.json"), config.to_string()).unwrap();
    let out = m.wrap(&[]);
    assert_eq!(code(&out), Some(0), "{}", stderr(&out));
    assert!(m.ran("cc").is_some());
}

#[test]
fn incremental_builds_compile_without_it() {
    for var in ["CARGO_INCREMENTAL", "CARGO_BUILD_INCREMENTAL"] {
        let m = Machine::new();
        let out = m.wrap(&[(var, "1"), ("CC_EXIT", "4")]);
        assert_eq!(code(&out), Some(4), "{var}");
        assert_eq!(m.ran("sccache"), None, "{var}");
        assert!(m.ran("cc").is_some(), "{var}");
        // Off is no reason.
        let m = Machine::new();
        m.wrap(&[(var, "0")]);
        assert!(m.ran("sccache").is_some(), "{var}=0");
    }
}

#[test]
fn a_cache_that_isnt_the_users_or_a_daemon_thats_down_compiles_without_it() {
    let shut = |m: &Machine, why: &str| {
        let out = m.wrap(&[("CC_EXIT", "5")]);
        assert_eq!(code(&out), Some(5), "{why}: {}", stderr(&out));
        assert_eq!(m.ran("sccache"), None, "{why}");
        assert!(m.ran("cc").is_some(), "{why}");
    };
    // No daemon: no socket, a plain file there, or a link to a socket gone.
    shut(&Machine::with(false), "no socket");
    let m = Machine::with(false);
    fs::write(m.path("root/sam/sccache.sock"), "").unwrap();
    shut(&m, "a file, not a socket");
    let m = Machine::with(false);
    std::os::unix::fs::symlink(m.path("run/sccache.sock"), m.path("root/sam/sccache.sock"))
        .unwrap();
    shut(&m, "a link to nothing");
    // No cache, or one someone else owns.
    let m = Machine::new();
    fs::remove_dir(m.path("root/sam/cache")).unwrap();
    shut(&m, "no cache");
    let m = Machine::new();
    fs::remove_dir(m.path("root/sam/cache")).unwrap();
    if rustix::process::geteuid().is_root() {
        fs::create_dir(m.path("root/sam/cache")).unwrap();
        std::os::unix::fs::chown(m.path("root/sam/cache"), Some(65534), Some(65534)).unwrap();
    } else {
        // Root's.
        std::os::unix::fs::symlink("/proc", m.path("root/sam/cache")).unwrap();
    }
    shut(&m, "someone else's cache");
}

#[test]
fn the_cache_is_the_one_user_names() {
    // Someone else's name: no cache of theirs here.
    let m = Machine::new();
    m.wrap_as(Some("kim"), &[]);
    assert_eq!(m.ran("sccache"), None);
    assert!(m.ran("cc").is_some());
    // No USER, or an empty one: id names the user.
    for user in [None, Some("")] {
        let m = Machine::new();
        let out = m.wrap_as(user, &[("WHO", "sam")]);
        assert_eq!(code(&out), Some(0), "{}", stderr(&out));
        assert!(m.ran("sccache").is_some(), "{user:?}");
        // And it's the user's own: id naming someone else is no cache.
        let m = Machine::new();
        m.wrap_as(user, &[("WHO", "kim")]);
        assert_eq!(m.ran("sccache"), None, "{user:?}");
    }
    // id can't say: no cache.
    let m = Machine::new();
    m.wrap_as(None, &[]);
    assert_eq!(m.ran("sccache"), None);
    assert!(m.ran("cc").is_some());
}

#[test]
fn a_config_it_cant_read_compiles_without_the_cache_and_says_so() {
    let m = Machine::new();
    fs::write(m.path("config.json"), r#"{"root": "/r"}"#).unwrap();
    let out = m.wrap(&[("CC_EXIT", "6")]);
    assert_eq!(code(&out), Some(6));
    assert!(
        stderr(&out).ends_with("; compiling without the cache\n"),
        "{}",
        stderr(&out)
    );
    assert!(stderr(&out).contains("missing field"), "{}", stderr(&out));
    assert!(m.ran("cc").is_some());
}

#[test]
fn what_it_cant_run() {
    let m = Machine::new();
    let bin = env!("CARGO_BIN_EXE_burrow-rustc-wrapper");
    // Nothing to compile.
    for words in [&[][..], &["config.json"]] {
        let out = Command::new(bin).args(words).output().unwrap();
        assert_eq!(code(&out), Some(2), "{words:?}");
        assert!(
            stderr(&out) == "usage: burrow-rustc-wrapper CONFIG COMPILER [ARGS...]\n",
            "{words:?}"
        );
    }
    // A compiler that isn't there, or can't be run, as a shell's exec says.
    for (compiler, want) in [
        ("/nonexistent/cc", 127),
        (stand_ins().join("not-runnable").to_str().unwrap(), 126),
    ] {
        let out = Command::new(bin)
            .args([m.path("config.json").to_str().unwrap(), compiler])
            .env("USER", "kim")
            .output()
            .unwrap();
        assert_eq!(code(&out), Some(want), "{compiler}");
        assert!(
            stderr(&out).starts_with(&format!("burrow-rustc-wrapper: {compiler}: ")),
            "{}",
            stderr(&out)
        );
    }
}
