//! The service behind an engine's socket: one call, as root, for as long as
//! its connection is open.

use std::io::{self, Read};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use conary_core::plugin::{self, Answer, Kind, Request, Response, Sandbox as Declared, Task};

use super::render::Attach;
use super::vm::Vm;
use super::{Frame, Header, receive, send, workspace};
use crate::{Agent, Engine, Plugin};

/// Held while a call makes its clone, so a caller going away never leaves
/// one that nothing has the name of.
static MAKING: Mutex<()> = Mutex::new(());

/// One call: `engine`'s, with `call` its directory. What went wrong is the
/// caller's to hear, so it never fails the service's unit.
pub fn serve(agent: &Agent, engine: &str, call: &Path) -> Result<(), String> {
    let header: Header = match receive(&mut io::stdin().lock()) {
        Ok(Some(header)) => header,
        Ok(None) => return Ok(()),
        Err(why) => {
            let _ = send(&mut io::stdout().lock(), &Frame::Failed(why));
            return Ok(());
        }
    };
    std::thread::spawn(|| {
        let mut rest = [0; 512];
        loop {
            match io::stdin().read(&mut rest) {
                Ok(0) => break,
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
        // The caller is gone. systemd ends whatever the call started, then
        // saves and deletes its clone (workspace::end).
        let _made = MAKING.lock().unwrap_or_else(PoisonError::into_inner);
        std::process::exit(0);
    });
    let last = match run(agent, engine, call, &header) {
        Ok(answer) => Frame::Answer(answer),
        Err(why) => Frame::Failed(why),
    };
    // Unheard means the caller is gone, which is no failure of the call's.
    let _ = send(&mut io::stdout().lock(), &last);
    Ok(())
}

fn say(text: String) {
    let _ = send(&mut io::stdout().lock(), &Frame::Log(text));
}

fn run(agent: &Agent, name: &str, call: &Path, header: &Header) -> Result<String, String> {
    let cfg = agent.cfg;
    let engine: &Engine = cfg.engine(name)?;
    let plugin: &Plugin = engine
        .plugin
        .as_ref()
        .ok_or_else(|| format!("{name} has no plugin"))?;
    let request: Request = serde_json::from_str(&header.request)
        .map_err(|e| format!("the request doesn't read: {e}"))?;
    if !matches!(request.task, Task::Describe | Task::Job { .. }) {
        return Err("the sandbox runs a plugin's describe and job requests alone".to_owned());
    }
    let described = matches!(request.task, Task::Describe);
    let ask = if described {
        header.request.clone()
    } else {
        serde_json::to_string(&Request::describe()).map_err(|e| e.to_string())?
    };
    let nothing = Attach::nothing(cfg);
    let vm = Vm {
        agent,
        engine,
        plugin,
        dir: call.join("describe"),
        clone: None,
        attach: &nothing,
    };
    let answer = vm.run(ask, &mut say)?;
    if described {
        return Ok(answer);
    }

    let (kinds, offered) = declared(&answer)?;
    if !runs_job(&kinds, &offered)? {
        return Err(format!(
            "{name}'s plugin says it brings a sandbox of its own: this runner runs no request \
             of it, and never runs a plugin on the host"
        ));
    }
    let grants = cfg.grants(name, &kinds, &offered)?;
    for refusal in grants.refusals() {
        say(format!("agent plugin: {refusal}\n"));
    }
    let credential = engine
        .credential_file
        .as_deref()
        .filter(|f| std::fs::metadata(f).is_ok_and(|m| m.len() > 0));
    let attach = Attach::granted(cfg, credential, &grants, header, &request.task)?;
    for (capability, why) in &attach.absent {
        say(format!(
            "agent plugin: {:?} granted and not attached: {why}\n",
            capability.name()
        ));
    }
    let made = match &attach.workspace {
        Some(w) => {
            let _making = MAKING.lock().unwrap_or_else(PoisonError::into_inner);
            Some(workspace::make(agent, call, name, w, attach.git_metadata)?)
        }
        None => None,
    };
    let vm = Vm {
        agent,
        engine,
        plugin,
        dir: call.join("work"),
        clone: made.as_ref().map(|(_, clone)| clone.clone()),
        attach: &attach,
    };
    let answer = vm.run(header.request.clone(), &mut say);
    if let Some((sandbox, _)) = &made {
        match workspace::save(agent, call, sandbox) {
            Ok(saved) => say(format!("agent plugin: {saved}\n")),
            Err(kept) if answer.is_ok() => return Err(kept),
            Err(kept) => say(format!("agent plugin: {kept}\n")),
        }
    }
    answer
}

/// What a describe answer declares, as far as grants read it. The caller's
/// own runner holds the rest of the answer to the contract.
fn declared(answer: &str) -> Result<(Vec<Kind>, plugin::Capabilities), String> {
    let bad = |e: &str| format!("the plugin's describe {e}");
    let response: Response =
        serde_json::from_str(answer).map_err(|e| bad(&format!("doesn't read: {e}")))?;
    if response.contract != Request::describe().contract {
        return Err(bad("isn't in describe's contract"));
    }
    let Answer::Describe {
        kinds,
        v2_kinds,
        v4_kinds,
        capabilities,
        ..
    } = response.answer
    else {
        return Err(bad("answers something else"));
    };
    let kinds = [kinds, v2_kinds, v4_kinds].concat();
    let offered = capabilities.ok_or_else(|| bad("declares no capabilities"))?;
    Ok((kinds, offered))
}

/// Whether this runner runs the job: for every plugin that doesn't bring a
/// sandbox of its own.
fn runs_job(kinds: &[Kind], offered: &plugin::Capabilities) -> Result<bool, String> {
    if !kinds.contains(&Kind::Job) {
        return Err("the plugin doesn't serve the job kind".to_owned());
    }
    Ok(match offered.sandbox {
        Declared::None | Declared::Needs => true,
        Declared::Brings => false,
    })
}

#[cfg(test)]
mod tests;
