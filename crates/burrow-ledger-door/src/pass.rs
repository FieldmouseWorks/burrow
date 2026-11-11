//! A pass over the repo's undo issues, at its door (burrow-door): each one
//! only authors wrote runs its `cn undo`, gets what that made in one
//! comment, and is closed; one anyone else wrote, or that asks for nothing
//! cn takes, gets a line saying why nothing ran, and is closed too. One
//! that found GitHub unavailable is left open for the next pass. The
//! comment tells every undo cn says it has, whether it finished, refused
//! or stopped partway: one that stopped tells what it opened before then.

use burrow_door::{Door, asked, clip, fenced};
use burrow_github::{Api, Error};
use serde_json::Value;

use crate::request::{Request, WORDS};
use crate::{Config, MARK};

/// What cn exits with when GitHub couldn't be asked.
const UNAVAILABLE: i32 = 75;

/// What cn exits with when the ledger refused what was asked: nothing was
/// undone. Any other failure may have stopped partway.
const REFUSED: i32 = 65;

/// The most of cn's own words a comment echoes.
const MAX_SAID: usize = 2_000;

/// What `cn` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// Its exit code; None when a signal ended it.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Runs `cn` with arguments, on the ledger.
pub trait Cn {
    fn run(&self, args: &[String]) -> Result<Ran, String>;
}

/// What a pass did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Done {
    /// The issues whose undo ran, and those turned away.
    pub answered: Vec<u64>,
    pub refused: Vec<u64>,
}

/// One pass.
pub fn pass(api: &mut dyn Api, cfg: &Config, cn: &dyn Cn) -> Result<Done, Error> {
    let door = Door {
        repo: &cfg.repo,
        label: &cfg.label,
        authors: &cfg.authors,
        what: "for an undo here",
    };
    let mut done = Done::default();
    let mut failed = Vec::new();
    for issue in door.issues(api)? {
        let n = issue.number;
        let text = match door.written(api, &issue) {
            Ok(Ok(text)) => text,
            Ok(Err(why)) => {
                match door.reply(api, n, &format!("{MARK}\n{why}"), "not_planned") {
                    Ok(()) => done.refused.push(n),
                    Err(e) => failed.push(e.context(format!("#{n}"))),
                }
                continue;
            }
            Err(e) => {
                failed.push(e.context(format!("#{n}")));
                continue;
            }
        };
        let request = match asked(&text).as_slice() {
            [line] => Request::parse(line),
            [] => Err(format!("Nothing was asked: {WORDS}.")),
            _ => Err(format!("One undo an issue: {WORDS}.")),
        };
        let (body, reason) = match request {
            Err(why) => (format!("{MARK}\nNothing was run. {why}"), "not_planned"),
            Ok(request) => {
                let ran = match cn.run(&request.args()) {
                    Ok(ran) => ran,
                    Err(e) => {
                        failed.push(Error::said(e).context(format!("#{n}: cn")));
                        continue;
                    }
                };
                if ran.code == Some(UNAVAILABLE) {
                    failed.push(
                        format!("#{n}: GitHub is unavailable to cn; asked again next pass").into(),
                    );
                    continue;
                }
                answer(cfg, &request, &ran)
            }
        };
        match door.reply(api, n, &body, reason) {
            Ok(()) if reason == "completed" => done.answered.push(n),
            Ok(()) => done.refused.push(n),
            Err(e) => failed.push(e.context(format!("#{n}"))),
        }
    }
    if failed.is_empty() {
        Ok(done)
    } else {
        Err(Error::aggregate(failed).context(&cfg.repo))
    }
}

/// The comment on what `cn undo` did, and how the issue closes.
fn answer(cfg: &Config, request: &Request, ran: &Ran) -> (String, &'static str) {
    let mut out = format!("{MARK}\n`cn {request}` on {}:\n", cfg.host);
    // cn tells its undos before anything else can stop it, so these are
    // what it has, however it ended.
    let any = rows(&ran.stdout, &mut out);
    let said = || fenced(&clip(ran.stderr.trim(), MAX_SAID));
    match ran.code {
        Some(0) => {
            if !any {
                out += "\ncn said nothing it opened.\n";
            }
            (out, "completed")
        }
        Some(REFUSED) => {
            out += &format!("\ncn refused, so nothing was undone:\n\n{}\n", said());
            (out, "not_planned")
        }
        _ => {
            let after = if any { ", after what's above" } else { "" };
            out += &format!(
                "\ncn stopped before it finished{after}:\n\n{}\n\nAsking again finds what it opened rather than opening more.\n",
                said()
            );
            (out, "not_planned")
        }
    }
}

/// cn's JSON lines, each undo and what's gone, onto `out`: whether any was.
fn rows(stdout: &str, out: &mut String) -> bool {
    let mut any = false;
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            *out += &format!("\n{}\n", fenced(&clip(line, MAX_SAID)));
            continue;
        };
        if let Some(url) = v["unrecorded"].as_str() {
            *out += &format!(
                "\n- {url}: opened, and cn stopped before recording it as the undo. Asking again records it.\n"
            );
            any = true;
            continue;
        }
        if let Some(gone) = ids(&v["gone"]) {
            *out += &format!(
                "\nTrunk already lacks what these brought, so nothing reverts them: {gone}.\n"
            );
            any = true;
            continue;
        }
        let Some(url) = v["url"].as_str() else {
            *out += &format!("\n{}\n", fenced(&clip(line, MAX_SAID)));
            continue;
        };
        any = true;
        let n = v["reverts"].as_array().map_or(0, Vec::len);
        let how = match (v["opened"].as_bool(), v["conflicts"].as_bool()) {
            (Some(true), Some(true)) => {
                "opened, conflicting with trunk: what came after builds on it"
            }
            (Some(true), _) => "opened",
            _ => "open already",
        };
        let changes = if n == 1 {
            "1 change".to_owned()
        } else {
            format!("{n} changes")
        };
        *out += &format!("\n- {url}: reverts {changes}, {how}.\n");
        if let Some(left) = ids(&v["left"]) {
            *out += &format!(
                "  Left out, since each one's revert conflicts with the rest: {left}. Ask again for an undo of their own.\n"
            );
        }
    }
    any
}

/// A list of change ids, as `a, b`: None when there are none.
fn ids(v: &Value) -> Option<String> {
    let ids: Vec<&str> = v.as_array()?.iter().filter_map(Value::as_str).collect();
    (!ids.is_empty()).then(|| {
        ids.iter()
            .map(|i| format!("`{i}`"))
            .collect::<Vec<_>>()
            .join(", ")
    })
}

#[cfg(test)]
#[path = "pass_tests.rs"]
mod tests;
