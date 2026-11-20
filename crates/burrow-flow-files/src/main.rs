//! `burrow-flow-files flow|policy|events|producers FILE...`: each file read
//! as the flow, `cn policy` or conaryd reads it, with Conary's own loaders
//! (`flow::Config::load`, `Policy::load`, `flow::schema::load` of conaryd's
//! events file, `events::Runtime`, and the flow's producer keys'
//! `Producers::load`), so a key Conary doesn't know, a value of the wrong
//! type or a malformed file is refused here as it would be by the machine's
//! cn. burrow's checks have no Conary flake, so this is how they hold the
//! files its modules render to Conary's types, and the machine's build
//! holds its producers file to it: `cn flow settings` reads the flow
//! config, not the producers file it names.
//!
//! `burrow-flow-files choice SETTINGS ROLE` prints, as JSON, what the
//! flow's configuration asks a plugin on ROLE to run with (its
//! `execution`): the role's engine, model and effort in the settings source
//! SETTINGS (`burrow_settings::Config`), as the plugin's jobs are held to
//! them (`burrow_producer::jobs::choice`).
use conary_core::Policy;
use conary_github::attest::service::Producers;
use conary_github::events::Runtime;
use conary_github::flow::Config;
use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let kind = args.next();
    let files: Vec<String> = args.collect();
    let read: fn(&Path) -> Result<(), String> = match kind.as_deref() {
        Some("flow") => |p| Config::load(p).map(drop).map_err(|e| e.to_string()),
        Some("policy") => |p| Policy::load(p).map(drop).map_err(|e| e.to_string()),
        Some("events") => |p| {
            let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
            conary_github::flow::schema::load::<Runtime>(&bytes)
                .map(drop)
                .map_err(|e| e.to_string())
        },
        Some("producers") => |p| Producers::load(p).map(drop),
        Some("choice") => return choice(&files),
        _ => return usage(),
    };
    if files.is_empty() {
        return usage();
    }
    let mut ok = true;
    for file in &files {
        if let Err(e) = read(Path::new(file)) {
            eprintln!("{file}: {e}");
            ok = false;
        }
    }
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn choice(args: &[String]) -> ExitCode {
    let [settings, role] = args else {
        return usage();
    };
    let chosen = std::fs::read(settings)
        .map_err(|e| format!("{settings}: {e}"))
        .and_then(|bytes| {
            serde_json::from_slice::<burrow_settings::Config>(&bytes)
                .map_err(|e| format!("{settings}: {e}"))
        })
        .and_then(|cfg| burrow_producer::jobs::choice(&cfg, role))
        .and_then(|c| serde_json::to_string(&c).map_err(|e| e.to_string()));
    match chosen {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{role}: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: burrow-flow-files flow|policy|events|producers FILE... | burrow-flow-files choice SETTINGS ROLE"
    );
    ExitCode::from(2)
}
