//! burrow-rustc-wrapper CONFIG COMPILER [ARGS...]: see lib.rs.

use std::ffi::OsString;
use std::io::Write as _;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode};

use burrow_rustc_wrapper::{After, Config, Plan, after, cached, direct, plan, unset_sccache};

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let [config, compile @ ..] = args.as_slice() else {
        return usage();
    };
    if compile.is_empty() {
        return usage();
    }
    match wrap(Path::new(config), compile) {
        Ok(run) => exec(run),
        Err(code) => ExitCode::from(code),
    }
}

fn usage() -> ExitCode {
    say("usage: burrow-rustc-wrapper CONFIG COMPILER [ARGS...]");
    ExitCode::from(2)
}

/// The compile to run in this process's place, or the exit its run through
/// sccache came to.
fn wrap(config: &Path, compile: &[OsString]) -> Result<Command, u8> {
    let cfg = match Config::read(config) {
        Ok(cfg) => cfg,
        Err(e) => {
            say(format_args!(
                "burrow-rustc-wrapper: {e}; compiling without the cache"
            ));
            return Ok(direct(compile));
        }
    };
    let Plan::Cached(base) = plan(&cfg, |name| std::env::var_os(name)) else {
        return Ok(direct(compile));
    };
    match after(cached(&cfg, &base, compile).status()) {
        After::Exit(code) => Err(code),
        After::Uncached => {
            let mut run = direct(compile);
            unset_sccache(&mut run);
            Ok(run)
        }
    }
}

/// Runs `run` in this process's place, as a shell's exec does: 127 when
/// there's no such program, 126 when it can't be run.
fn exec(mut run: Command) -> ExitCode {
    let e = run.exec();
    say(format_args!(
        "burrow-rustc-wrapper: {}: {e}",
        run.get_program().to_string_lossy()
    ));
    ExitCode::from(if e.kind() == std::io::ErrorKind::NotFound {
        127
    } else {
        126
    })
}

/// One line for stderr. eprintln! panics when stderr can't be written, which
/// would trade the compile's exit code for a panic's.
fn say(line: impl std::fmt::Display) {
    let _ = writeln!(std::io::stderr(), "{line}");
}
