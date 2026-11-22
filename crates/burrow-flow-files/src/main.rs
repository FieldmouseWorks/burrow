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
//! `burrow-flow-files render MANIFEST` writes each repo's flow config as
//! the flow reads it on the machine: its base, as the build rendered it,
//! with each role-run plugin's choice (`execution`) as the settings say now
//! ([`render`]).
//!
//! `burrow-flow-files judge-key CONFIG SEAL` loads CONFIG as the flow does
//! (`flow::Config::load`, which reads a key its `judge_key_file` names) and
//! succeeds only if its `judge_key` is the key the file SEAL holds (the
//! judge's seal key), printing neither.
use conary_core::Policy;
use conary_github::attest::service::Producers;
use conary_github::events::Runtime;
use conary_github::flow::{Config, SealKey};
use std::path::Path;
use std::process::ExitCode;

mod render;

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
        Some("render") => return render(&files),
        Some("judge-key") => return judge_key(&files),
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

fn render(args: &[String]) -> ExitCode {
    let [manifest] = args else {
        return usage();
    };
    let read = std::fs::read(manifest)
        .map_err(|e| format!("{manifest}: {e}"))
        .and_then(|bytes| {
            serde_json::from_slice::<render::Manifest>(&bytes)
                .map_err(|e| format!("{manifest}: {e}"))
        })
        .and_then(|m| render::render(&m).map(|n| (n, m.out)));
    match read {
        Ok((n, out)) => {
            println!("rendered {n} flow configs in {}", out.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn judge_key(args: &[String]) -> ExitCode {
    let [config, seal] = args else {
        return usage();
    };
    let loaded = Config::load(Path::new(config))
        .map(|c| c.settings.judge_key)
        .map_err(|e| format!("{config}: {e}"));
    let sealed = SealKey::read(Path::new(seal));
    match (loaded, sealed) {
        (Ok(Some(loaded)), Ok(sealed)) if loaded == sealed => ExitCode::SUCCESS,
        (Ok(Some(_)), Ok(_)) => {
            eprintln!("{config}: its judge_key isn't the one {seal} holds");
            ExitCode::FAILURE
        }
        (Ok(None), _) => {
            eprintln!("{config}: it has no judge_key");
            ExitCode::FAILURE
        }
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: burrow-flow-files flow|policy|events|producers FILE... | burrow-flow-files render MANIFEST | burrow-flow-files judge-key CONFIG SEAL"
    );
    ExitCode::from(2)
}
