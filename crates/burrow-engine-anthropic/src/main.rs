//! The `burrow-engine-anthropic` `conary-plugin v5` `job` executable: reads one
//! JSON request on stdin, runs Anthropic's `claude` or `claude-api` mode, and
//! writes one JSON response on stdout.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use burrow_engine_anthropic::plugin::{EXIT_FAILURE, PluginConfig, run_stdio};

fn load_config(args: &[String]) -> Result<PluginConfig, String> {
    match args {
        [] => PluginConfig::from_env(),
        [flag, path] if flag == "--config" => PluginConfig::from_file(Path::new(path)),
        _ => Err("usage: burrow-engine-anthropic [--config PATH]".to_owned()),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = match load_config(&args) {
        Ok(cfg) => cfg,
        Err(why) => {
            eprintln!("burrow-engine-anthropic: {why}");
            return ExitCode::from(EXIT_FAILURE);
        }
    };
    let mut stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    ExitCode::from(run_stdio(&config, &mut stdin, &mut stdout, &mut stderr))
}
