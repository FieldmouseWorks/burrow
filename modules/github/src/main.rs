//! gh-token: GitHub App installation tokens for the processes that ask, from
//! a key they can't read.
//!
//!   gh-token answer --app-id ID --key FILE --cache DIR [--api URL]
//!       answer one request on stdin (answer.rs): the service's side
//!   gh-token get OWNER[/REPO]
//!   gh-token git-credential get|store|erase
//!   gh-token gh GH [ARGS...]
//!       the askers' side (client.rs), through the socket GH_TOKEN_SOCKET
//!
//! A token comes back for an account (every repo the App's installation on
//! it reaches) or for one repo of it. It's good for at least ten minutes,
//! and for an hour at most. Exit status 75 is "ask again later": GitHub, the
//! network or the service wasn't there.

mod account;
mod answer;
mod app;
mod cache;
mod client;
mod command;
mod error;
#[cfg(test)]
mod fake;
mod target;

use std::io;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use error::Error;

const USAGE: &str = "usage: gh-token answer --app-id ID --key FILE --cache DIR [--api URL]
       gh-token get OWNER[/REPO]
       gh-token git-credential get|store|erase
       gh-token gh GH [ARGS...]";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("gh-token: {e}");
            ExitCode::from(e.code())
        }
    }
}

fn run(args: &[String]) -> Result<(), Error> {
    let socket = std::env::var_os("GH_TOKEN_SOCKET")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let io = |e: io::Error| Error::unavailable(e.to_string());
    match args.first().map(String::as_str) {
        Some("answer") => {
            let config = answer_config(&args[1..])?;
            answer::answer(&config, io::stdin().lock(), io::stdout().lock(), now()).map_err(io)
        }
        Some("get") if args.len() == 2 => {
            let socket = socket.ok_or_else(|| Error::refused("GH_TOKEN_SOCKET isn't set"))?;
            println!("{}", client::fetch(&socket, &args[1])?);
            Ok(())
        }
        Some("git-credential") if args.len() == 2 => client::git_credential(
            &args[1],
            io::stdin().lock(),
            io::stdout().lock(),
            socket.as_deref(),
        )
        .map_err(io),
        Some("gh") if args.len() >= 2 => Err(client::gh(&args[1], &args[2..], socket.as_deref())),
        _ => Err(Error::refused(USAGE)),
    }
}

fn answer_config(args: &[String]) -> Result<answer::Config, Error> {
    let mut config = answer::Config {
        api: "https://api.github.com".into(),
        proxy: ureq::Proxy::try_from_env(),
        app_id: 0,
        key: PathBuf::new(),
        cache: PathBuf::new(),
    };
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        let value = it.next().ok_or_else(|| Error::refused(USAGE))?;
        match flag.as_str() {
            "--api" => config.api.clone_from(value),
            "--app-id" => {
                config.app_id = value
                    .parse()
                    .map_err(|_| Error::refused("--app-id is a number"))?
            }
            "--key" => config.key = value.into(),
            "--cache" => config.cache = value.into(),
            _ => return Err(Error::refused(USAGE)),
        }
    }
    if config.app_id == 0
        || config.key.as_os_str().is_empty()
        || config.cache.as_os_str().is_empty()
    {
        return Err(Error::refused(USAGE));
    }
    Ok(config)
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
