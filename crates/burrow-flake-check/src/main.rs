//! The shared gate and negative runner. No Rust dependencies: callers compile
//! these pinned sources directly with rustc, without a registry or cargo build.
//! jq is the JSON codec (already required by the workflow), not the scheduler.
mod build;
mod gate;
mod memory;
mod negative;
mod plan;
mod snapshot;
mod wait;

use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};
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
fn cmd(p: &str, args: &[&str]) -> Command {
    let mut c = Command::new(p);
    c.args(args);
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
fn checked(c: Command) -> Result<Vec<u8>> {
    let o = output(c)?;
    if !o.status.success() {
        io::stderr().write_all(&o.stderr)?;
        return Err(err(format!("command failed: {}", o.status)));
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
fn run(mut c: Command) -> Result<bool> {
    Ok(c.status()?.success())
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
    if mode == "wait" {
        match wait::wait() {
            Ok(_) => return,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    }
    if mode == "--worker" {
        let result = match env::args().nth(2).as_deref() {
            Some("gate") => gate::gate(),
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
    let result = (|| -> Result<i32> {
        if mode != "gate" && mode != "negative" {
            return Err(err("expected gate or negative"));
        }
        use std::io::Read;
        let mut log = fs::File::create(temp().join(if mode == "gate" {
            "flake.log"
        } else {
            "negative.log"
        }))?;
        let mut child = cmd(
            "bash",
            &[
                "-c",
                "exec \"$@\" 2>&1",
                "bash",
                s(&env::current_exe()?),
                "--worker",
                &mode,
            ],
        )
        .stdout(Stdio::piped())
        .spawn()?;
        let mut pipe = child.stdout.take().unwrap();
        let mut buf = [0; 8192];
        loop {
            let n = pipe.read(&mut buf)?;
            if n == 0 {
                break;
            }
            log.write_all(&buf[..n])?;
            io::stdout().write_all(&buf[..n])?;
        }
        Ok(exit_code(child.wait()?))
    })();
    match result {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
