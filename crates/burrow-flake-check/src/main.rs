//! The shared gate and negative runner, and gate-reuse's lookup. No Rust
//! dependencies: callers compile these pinned sources directly with rustc,
//! without a registry or cargo build.
//! jq is the JSON codec (already required by the workflow), not the scheduler.
mod build;
mod builds;
mod flaky;
mod gate;
mod memory;
mod negative;
mod plan;
mod reuse;
mod snapshot;
mod source;
mod verdict;

use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};
use verdict::{failed, judged};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn err(s: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    s.into().into()
}
fn var(k: &str) -> String {
    env::var(k).unwrap_or_default()
}
fn temp() -> PathBuf {
    PathBuf::from(var("RUNNER_TEMP"))
}
/// Where git finds a repository, when the environment says: once gate-reuse's
/// private source is entered, no command inherits these (source.rs).
const GIT_LOCATIONS: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
];
static PRIVATE_SOURCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn cmd(p: &str, args: &[&str]) -> Command {
    let mut c = Command::new(p);
    c.args(args);
    if PRIVATE_SOURCE.load(std::sync::atomic::Ordering::SeqCst) {
        for k in GIT_LOCATIONS {
            c.env_remove(k);
        }
    }
    c
}
fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
}
fn output(mut c: Command) -> Result<Output> {
    Ok(c.output()?)
}
fn program(c: &Command) -> String {
    c.get_program().to_string_lossy().into_owned()
}
fn checked(c: Command) -> Result<Vec<u8>> {
    let program = program(&c);
    let o = output(c)?;
    if !o.status.success() {
        io::stderr().write_all(&o.stderr)?;
        return Err(err(format!("{program}: command failed: {}", o.status)));
    }
    Ok(o.stdout)
}
// Git paths and config values may themselves end in whitespace, including
// newlines. Remove exactly the command's output terminator, never path data.
fn text_output(bytes: Vec<u8>) -> Result<String> {
    let mut text = String::from_utf8(bytes)?;
    if text.ends_with('\n') {
        text.pop();
    }
    Ok(text)
}
fn string(c: Command) -> Result<String> {
    text_output(checked(c)?)
}
/// Whether it passed. One stopped before it could say didn't, and is how
/// the gate broke (verdict.rs), with whatever else there is to run still run.
fn run(mut c: Command) -> Result<bool> {
    let status = c.status()?;
    Ok(verdict::passed(&program(&c), status).unwrap_or_else(|e| {
        verdict::broke(&*e);
        false
    }))
}
fn save(path: &Path, bytes: impl AsRef<[u8]>) -> Result<()> {
    fs::write(path, bytes).map_err(|e| err(format!("couldn't write {}: {e}", path.display())))
}
/// A command whose stderr goes where its stdout does: one pipe, in the
/// order it said them.
fn merged(p: &str, args: &[&str]) -> Command {
    let mut c = cmd("bash", &["-c", "exec \"$@\" 2>&1", "bash", p]);
    c.args(args);
    c
}
/// How a command [`tee`] ran ended: its exit code and the last it said.
struct Ended {
    code: i32,
    last: String,
}
/// Run it, copying each line it says to `out` and to `log` as it comes, so a
/// long build is watched as it goes and its log kept whole. One that can't
/// be written doesn't stop the other, or the command, which is always read
/// to its end and waited for; then that's the error.
fn tee(mut c: Command, log: &mut dyn Write, out: &mut dyn Write) -> Result<Ended> {
    use std::io::BufRead;
    let mut child = c.stdout(Stdio::piped()).spawn()?;
    let mut pipe = io::BufReader::new(child.stdout.take().ok_or_else(|| err("no pipe"))?);
    let (mut line, mut last) = (Vec::new(), Vec::new());
    let (mut unlogged, mut unshown) = (None, None);
    let read = loop {
        line.clear();
        match pipe.read_until(b'\n', &mut line) {
            Ok(0) => break Ok(()),
            Ok(_) => {}
            Err(e) => break Err(e),
        }
        if unlogged.is_none() {
            unlogged = log.write_all(&line).err();
        }
        if unshown.is_none() {
            unshown = out.write_all(&line).err();
        }
        last.clone_from(&line);
    };
    drop(pipe);
    let status = child.wait()?;
    read?;
    if let Some(e) = unlogged {
        return Err(err(format!("couldn't write its log: {e}")));
    }
    if let Some(e) = unshown {
        return Err(err(format!("couldn't write the step's output: {e}")));
    }
    Ok(Ended {
        code: exit_code(status),
        last: String::from_utf8_lossy(&last).trim_end().to_owned(),
    })
}
/// [`tee`], the command given `limit` seconds: past them it and every
/// process it started are stopped (`timeout` signals its whole process
/// group, and kills it a minute later if it's still there), and that's the
/// error, so a run that ran out never reads as one that said how it ended.
fn watch(c: Command, limit: u32, log: &mut dyn Write, out: &mut dyn Write) -> Result<Ended> {
    let mut bounded = cmd("timeout", &["--kill-after=60", &limit.to_string()]);
    bounded.arg(c.get_program()).args(c.get_args());
    for (k, v) in c.get_envs() {
        match v {
            Some(v) => bounded.env(k, v),
            None => bounded.env_remove(k),
        };
    }
    let started = std::time::Instant::now();
    let ended = tee(bounded, log, out)?;
    // 124 is `timeout`'s own; a kill after it is a signal's, once the
    // limit has passed.
    let ran_out =
        ended.code == 124 || (ended.code >= 128 && started.elapsed().as_secs() >= u64::from(limit));
    if ran_out {
        return Err(err(format!(
            "it ran past its {} minutes, so it was stopped before it could say: it can't tell",
            limit / 60
        )));
    }
    Ok(ended)
}
fn show(o: &Output) -> Result<()> {
    io::stdout().write_all(&o.stdout)?;
    io::stderr().write_all(&o.stderr)?;
    Ok(())
}
fn s(p: &Path) -> &str {
    p.to_str().expect("UTF-8 path")
}
fn jq(input: &[u8], filter: &str) -> Result<String> {
    let mut child = cmd("jq", &["-cr", filter])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(input)?;
    let o = child.wait_with_output()?;
    if !o.status.success() {
        return Err(err(String::from_utf8_lossy(&o.stderr).into_owned()));
    }
    text_output(o.stdout)
}
fn quote(v: &str) -> String {
    // JSON is a Nix string too, after escaping interpolation's dollar.
    let mut out = String::from("\"");
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn nix_string(v: &str) -> String {
    quote(v).replace('$', "\\$")
}
/// A build that prints nothing for this long has stopped making progress:
/// nix kills it (its `max-silent-time`) and the gate fails, rather than hold
/// its runner until the run's own limit ([`RUN_SECONDS`]) runs out (#1102). The rest of the
/// builds go on (`--keep-going`), and a push's leg still keeps what they
/// built. burrow's longest quiet stretch in a build that went on to pass
/// was 14 to 16 minutes, codex linking on a hosted runner.
const SILENT_SECONDS: u32 = 60 * 60;
/// How long a gate or negative run may take, its evaluations and builds
/// together: past it the worker and everything it started are stopped, and
/// the run can't tell (a gate's `error`, a negative with no result), with
/// why said, rather than hold its runner until the job is cut off with
/// nothing said (#1488). Every build is bounded on its own, but a run is
/// many of them, one after another, and its evaluations are bounded by
/// nothing else. The longest jobs that passed, in 40 to 200 of each caller's
/// (2026-11-23): a negative 112 minutes and a leg 87, burrow's on hosted
/// runners, and remi's fresh check 99; every other caller's 30 at most. So
/// this is the longest with room for a slower runner, and for a wait for a
/// heavy slot, which counts against it as the job's own did. flake-check.yml's
/// jobs each give their run this and what their other steps take
/// (burrow-workflows' timeouts test holds them to it).
const RUN_SECONDS: u32 = 180 * 60;
fn lines(v: &str) -> Vec<String> {
    v.lines().map(str::to_owned).collect()
}
fn nul(v: &[u8]) -> Result<Vec<String>> {
    v.split(|b| *b == 0)
        .filter(|x| !x.is_empty())
        .map(|x| Ok(String::from_utf8(x.to_vec())?))
        .collect()
}
fn root(drv: &str, to: &Path) -> Result<()> {
    checked(cmd(
        "nix-instantiate",
        &[
            "--add-root",
            s(to),
            "--argstr",
            "drv",
            drv,
            "--expr",
            "{ drv }: { type = \"derivation\"; name = \"root\"; outputName = \"out\"; drvPath = drv; }",
        ],
    ))?;
    Ok(())
}
struct Work(PathBuf);
impl Work {
    fn new(prefix: &str) -> Result<Self> {
        Ok(Self(PathBuf::from(string(cmd(
            "mktemp",
            &["-d", s(&temp().join(format!("{prefix}.XXXXXX")))],
        ))?)))
    }
}
impl Drop for Work {
    fn drop(&mut self) {
        let _ = cmd("chmod", &["-R", "u+w", "--", s(&self.0)])
            .stderr(Stdio::null())
            .status();
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn system() -> Result<String> {
    let v = string(cmd("nix", &["config", "show", "system"]))?;
    if v.is_empty()
        || !v
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(err(format!("can't evaluate for system {v}")));
    }
    Ok(v)
}
/// The nix config every build the worker starts reads: a caller's first,
/// then a build's silence limit, and, in a run that may write the build
/// store, what keeps a flake's own config out of it (builds.rs).
fn worker_config(caller: &str, writes: bool) -> String {
    let mut c = format!("{caller}\nmax-silent-time = {SILENT_SECONDS}");
    if writes {
        c.push('\n');
        c.push_str(builds::NIX_CONFIG);
    }
    c
}
fn main() {
    let mode = env::args().nth(1).unwrap_or_default();
    // A child performs the work so its stdout and stderr share one pipe, as
    // tee did; the parent preserves its exit status and writes the attested log.
    if mode == "--build" {
        let args: Vec<_> = env::args().skip(2).collect();
        let result = build::checks(None, &args[1..], Path::new(&args[0]));
        match result {
            Ok(true) => return,
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // The wait step runs its own plan and prints to the step's log.
    if mode == "cold" {
        match gate::plan() {
            Ok(true) => return,
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    // gate-reuse's lookup, before anything is built; its answer goes to
    // the step's outputs.
    if mode == "reuse" {
        match reuse::reuse() {
            Ok(true) => return,
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("gate-reuse: can't tell ({e}), so the gate runs");
                std::process::exit(1);
            }
        }
    }
    if mode == "--worker" {
        let result = match env::args().nth(2).as_deref() {
            Some("gate") => std::process::exit(verdict::worker(gate::gate())),
            Some("negative") => negative::negative(),
            _ => Err(err("expected gate or negative")),
        };
        match result {
            Ok(true) => (),
            Ok(false) => std::process::exit(1),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if mode != "gate" && mode != "negative" {
        eprintln!("expected gate or negative");
        std::process::exit(1);
    }
    let gate = mode == "gate";
    if gate {
        verdict::clear();
    }
    let watched = (|| -> Result<Ended> {
        let path = temp().join(if gate { "flake.log" } else { "negative.log" });
        let mut log = fs::File::create(&path)
            .map_err(|e| err(format!("couldn't create {}: {e}", path.display())))?;
        let mut c = merged(s(&env::current_exe()?), &["--worker", &mode]);
        // Every build the worker starts, its own and the nix and heavy
        // processes it runs, reads this; a caller's settings come first.
        c.env(
            "NIX_CONFIG",
            worker_config(&var("NIX_CONFIG"), builds::writes()),
        );
        watch(c, RUN_SECONDS, &mut log, &mut io::stdout())
    })();
    if gate {
        std::process::exit(verdict::settle(watched));
    }
    match watched {
        Ok(ended) => std::process::exit(ended.code),
        Err(e) => {
            // What it found has no whole log to be signed over: it can't tell.
            let _ = fs::remove_file(temp().join("negative.json"));
            let _ = writeln!(
                io::stdout(),
                "::error title=The negative run couldn't tell::{}",
                e.to_string().replace('%', "%25")
            );
            std::process::exit(1);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    /// A sink with no room (ENOSPC is 28).
    struct Full;
    impl Write for Full {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::from_raw_os_error(28))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    fn three_lines() -> Command {
        cmd("sh", &["-c", "echo one; echo two; echo three; exit 1"])
    }
    #[test]
    fn a_full_log_is_the_error_once_the_command_has_said_all() {
        let mut out = Vec::new();
        let Err(e) = tee(three_lines(), &mut Full, &mut out) else {
            panic!("a full log passed");
        };
        assert!(e.to_string().contains("No space left on device"), "{e}");
        assert!(!e.is::<verdict::Failed>());
        assert_eq!(out, b"one\ntwo\nthree\n");
    }
    #[test]
    fn a_full_step_output_is_the_error_and_the_log_is_whole() {
        let mut log = Vec::new();
        let Err(e) = tee(three_lines(), &mut log, &mut Full) else {
            panic!("a full output passed");
        };
        assert!(e.to_string().contains("No space left on device"), "{e}");
        assert_eq!(log, b"one\ntwo\nthree\n");
    }
    /// A run past its limit is stopped, with what it started: here a
    /// grandchild that holds the pipe open, as a build's nix does, so
    /// stopping the worker alone would never end the read.
    #[test]
    fn a_run_past_its_limit_is_stopped_whole_and_cant_tell() {
        let started = std::time::Instant::now();
        let mut out = Vec::new();
        let c = cmd("sh", &["-c", "echo started; sleep 600 & sleep 600"]);
        let Err(e) = watch(c, 1, &mut Vec::new(), &mut out) else {
            panic!("a run past its limit said how it ended");
        };
        assert!(e.to_string().contains("ran past its 0 minutes"), "{e}");
        assert!(!e.is::<verdict::Failed>());
        assert_eq!(out, b"started\n");
        assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
    }
    #[test]
    fn a_run_inside_its_limit_ends_as_it_said() {
        let ended = watch(three_lines(), 60, &mut Vec::new(), &mut Vec::new()).unwrap();
        assert_eq!((ended.code, ended.last.as_str()), (1, "three"));
    }
    #[test]
    fn only_a_failure_said_is_a_checks() {
        assert_eq!(verdict::worker(Ok(false)), 1);
        assert_eq!(verdict::worker(Err(failed("refused"))), 1);
        assert_eq!(verdict::worker(Err(err("tar failed"))), verdict::BROKEN);
        let full = io::Error::from_raw_os_error(28);
        assert_eq!(verdict::worker(Err(full.into())), verdict::BROKEN);
        assert!(!run(cmd("sh", &["-c", "exit 100"])).unwrap());
        assert!(!run(cmd("sh", &["-c", "kill -9 $$"])).unwrap());
        // Stopped, so the failure that followed isn't a check's.
        assert_eq!(verdict::worker(Ok(false)), verdict::BROKEN);
    }
}
