use burrow_settings::Config;
use std::{fs, process::ExitCode};
fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let result = (|| {
        if args == ["reference"] {
            return Ok(burrow_settings::reference::markdown());
        }
        if args.len() < 2 {
            return Err(
                "usage: burrow-settings CONFIG COMMAND... | burrow-settings reference".into(),
            );
        }
        let path = args.remove(0);
        if args == ["door"] {
            let cfg: burrow_settings::door::DoorConfig =
                serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?;
            burrow_github::deferred::configured(|api| burrow_settings::door::pass(api, &cfg))?;
            return Ok(String::new());
        }
        let cfg: Config = serde_json::from_slice(&fs::read(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if let [verb, repo] = args.as_slice()
            && verb == "decision"
        {
            return serde_json::to_string(&cfg.decision(repo)?).map_err(|e| e.to_string());
        }
        if let [verb, repo] = args.as_slice()
            && verb == "allows"
        {
            return cfg.allows(repo).and_then(|ok| {
                if ok {
                    Ok(String::new())
                } else {
                    Err("automated work paused".into())
                }
            });
        }
        cfg.command(
            &args,
            &std::env::var("USER").unwrap_or_else(|_| "host".into()),
        )
    })();
    match result {
        Ok(s) => {
            print!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("settings: {e}");
            ExitCode::FAILURE
        }
    }
}
