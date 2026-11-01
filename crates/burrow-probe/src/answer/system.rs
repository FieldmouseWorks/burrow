//! What systemd, the journal, proj and the system's own links say: `units`,
//! `journal`, `rc`, `queue`, `deployed` and `oom`.

use std::collections::BTreeMap;
use std::fs;

use burrow_events::glob;
use serde_json::Value;

use super::{JOURNAL_LINES, Machine, or_none, utc};
use crate::query::Unit;

/// The flags every journal read takes: one line an entry, in UTC's ISO
/// form, no paging, no hints.
const JOURNAL: [&str; 6] = [
    "-o",
    "short-iso",
    "--no-pager",
    "--no-hostname",
    "-q",
    "--utc",
];

/// The flags every unit listing takes.
const LIST: [&str; 4] = ["--plain", "--no-legend", "--no-pager", "--full"];

/// `units`: what failed, and what's under way, for the system and the user.
pub fn units(m: &Machine) -> Result<String, String> {
    let ctl = &m.cfg.tools.systemctl;
    let mut out = String::new();
    for user in [false, true] {
        let who = if user {
            format!("{}'s user manager", m.cfg.user)
        } else {
            "The system".to_string()
        };
        let with = |verb: &[&'static str]| -> Vec<&'static str> {
            let mut a = if user { vec!["--user"] } else { vec![] };
            a.extend(verb);
            a
        };
        let failed = with(&["list-units", "--failed", LIST[0], LIST[1], LIST[2], LIST[3]]);
        let jobs = with(&["list-jobs", "--no-legend", "--no-pager", "--full"]);
        let f = m.output(ctl, &failed, user, &[]);
        let j = m.output(ctl, &jobs, user, &[]);
        out += &format!(
            "{who}, failed units:\n{}\n{who}, jobs under way:\n{}\n",
            f.map_or_else(|e| e, |s| or_none(&s)),
            j.map_or_else(|e| e, |s| or_none(&s)),
        );
    }
    Ok(out)
}

/// `journal UNIT SINCE UNTIL`: the last JOURNAL_LINES lines of its span, if
/// the unit is on the allowlist.
pub fn journal(m: &Machine, unit: &Unit, since: i64, until: i64) -> Result<String, String> {
    let on = |globs: &[String]| globs.iter().any(|g| glob::matches(g, unit.as_str()));
    // Each manager whose allowlist names it: a name on both (proj's jobs, say)
    // is read from both, in one read, so the span and the line limit are the
    // two logs' together. journalctl ORs its unit matches, and a user unit is
    // the probe's own user's.
    let mut args = Vec::new();
    if on(&m.cfg.journal.units) {
        args.extend(["-u", unit.as_str()]);
    }
    if on(&m.cfg.journal.user_units) {
        args.extend(["--user-unit", unit.as_str()]);
    }
    if args.is_empty() {
        return Err(format!(
            "{unit} isn't on the journal allowlist (system units: {}; user units: {})",
            list(&m.cfg.journal.units),
            list(&m.cfg.journal.user_units),
        ));
    }
    if since >= until {
        return Err(format!("{} isn't before {}", utc(since), utc(until)));
    }
    // A time names a whole second, and the span takes in all of its last
    // one: `--until` is to the microsecond, so `@UNTIL` alone would cut what
    // was logged in the second `now` names, before the probe read it.
    let (s, u, n) = (
        format!("@{since}"),
        format!("@{until}.999999"),
        JOURNAL_LINES.to_string(),
    );
    args.extend(["--since", &s, "--until", &u, "-n", &n]);
    args.extend(JOURNAL);
    let lines = m.output(&m.cfg.tools.journalctl, &args, false, &[])?;
    let count = lines.lines().count();
    let head = if count >= JOURNAL_LINES {
        format!(
            "{unit}, {} to {}: its last {JOURNAL_LINES} lines (ask a shorter span for the rest)\n",
            utc(since),
            utc(until)
        )
    } else {
        format!("{unit}, {} to {}: {count} lines\n", utc(since), utc(until))
    };
    Ok(head + &lines)
}

fn list(globs: &[String]) -> String {
    if globs.is_empty() {
        "none".into()
    } else {
        globs.join(", ")
    }
}

/// `rc`: each Remote Control server's state, memory, restarts and how long
/// it's been up, from the user's manager.
pub fn rc(m: &Machine) -> Result<String, String> {
    let ctl = &m.cfg.tools.systemctl;
    let mut args = vec!["--user", "list-units", "claude-rc@*", "--all"];
    args.extend(LIST);
    let listed = m.output(ctl, &args, true, &[])?;
    let units: Vec<&str> = listed
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|u| u.starts_with("claude-rc@") && u.ends_with(".service"))
        .collect();
    if units.is_empty() {
        return Ok("No Remote Control servers.\n".into());
    }
    let mut args = vec![
        "--user",
        "show",
        "-p",
        "Id,ActiveState,SubState,MemoryCurrent,MemoryPeak,NRestarts,ActiveEnterTimestamp",
    ];
    args.extend(&units);
    let shown = m.output(ctl, &args, true, &[])?;
    let mut out = format!(
        "{} Remote Control server{} (state, memory now, peak, restarts, up since):\n",
        units.len(),
        if units.len() == 1 { "" } else { "s" }
    );
    for block in shown.split("\n\n") {
        let p: BTreeMap<&str, &str> = block.lines().filter_map(|l| l.split_once('=')).collect();
        let Some(id) = p.get("Id") else { continue };
        let bytes = |k: &str| {
            p.get(k)
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|b| *b != u64::MAX)
                .map_or("-".to_string(), super::size)
        };
        out += &format!(
            "  {:<28} {:<18} {:>7} {:>7} {:>3}  {}\n",
            server(id),
            format!(
                "{}/{}",
                p.get("ActiveState").unwrap_or(&"?"),
                p.get("SubState").unwrap_or(&"?")
            ),
            bytes("MemoryCurrent"),
            bytes("MemoryPeak"),
            p.get("NRestarts").unwrap_or(&"?"),
            p.get("ActiveEnterTimestamp")
                .filter(|t| !t.is_empty())
                .unwrap_or(&"-"),
        );
    }
    Ok(out)
}

/// claude-rc@NAME.service's NAME, unescaped (systemd-escape's `\xNN`).
fn server(unit: &str) -> String {
    let name = unit
        .strip_prefix("claude-rc@")
        .and_then(|n| n.strip_suffix(".service"))
        .unwrap_or(unit);
    let mut out = String::new();
    let mut rest = name;
    while let Some(i) = rest.find("\\x") {
        out += &rest[..i];
        match rest
            .get(i + 2..i + 4)
            .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            Some(b) if b.is_ascii() => {
                out.push(b as char);
                rest = &rest[i + 4..];
            }
            _ => {
                out += "\\x";
                rest = &rest[i + 2..];
            }
        }
    }
    out + rest
}

/// `queue`: proj's jobs, as `proj status` lists them.
pub fn queue(m: &Machine) -> Result<String, String> {
    let proj = m.cfg.proj.as_ref().ok_or("proj isn't on here")?;
    m.output(proj, &["status"], true, &[])
        .map(|s| or_none(&s) + "\n")
}

/// `deployed`: the system that runs and the one that booted, its
/// generation, version and revision, its inputs' revisions, and what
/// HOST-update last found live.
pub fn deployed(m: &Machine) -> Result<String, String> {
    let link = |p: &str| {
        fs::read_link(m.path(p))
            .ok()
            .map(|t| t.display().to_string())
    };
    let running = link("/run/current-system").ok_or("/run/current-system isn't a link")?;
    let mut out = format!("Running: {running}\n");
    match link("/run/booted-system") {
        Some(b) if b == running => out += "Booted: the same\n",
        Some(b) => out += &format!("Booted: {b}\n"),
        None => out += "Booted: unknown\n",
    }
    if let Some(generation) = link("/nix/var/nix/profiles/system") {
        let at = fs::symlink_metadata(m.path("/nix/var/nix/profiles").join(&generation))
            .and_then(|md| md.modified())
            .ok()
            .and_then(|t| jiff::Timestamp::try_from(t).ok())
            .map_or("?".into(), |t| utc(t.as_second()));
        out += &format!("Generation: {generation}, made {at}\n");
    }
    let version = m.path("/run/current-system/sw/bin/nixos-version");
    match m.output(&version, &["--json"], false, &[]) {
        Ok(v) => {
            let v: Value = serde_json::from_str(&v).unwrap_or_default();
            let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
            out += &format!(
                "NixOS {}, configuration revision {}\n",
                s("nixosVersion"),
                s("configurationRevision")
            );
        }
        Err(e) => out += &format!("NixOS version: {e}\n"),
    }
    if let Ok(text) = fs::read_to_string(m.path("/etc/burrow/inputs.json")) {
        let inputs: BTreeMap<String, Value> = serde_json::from_str(&text).unwrap_or_default();
        out += "Inputs:\n";
        for (name, i) in inputs {
            let rev = i.get("rev").and_then(Value::as_str).unwrap_or("?");
            let when = i
                .get("lastModified")
                .and_then(Value::as_i64)
                .map_or("?".into(), utc);
            out += &format!("  {name:<16} {rev}  {when}\n");
        }
    }
    let live = m.cfg.state_dir.join("live.json");
    match fs::read_to_string(&live)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    {
        Some(l) => {
            let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("?").to_string();
            let same = if s("running") == running {
                ", which is what runs"
            } else {
                ", not what runs now"
            };
            out += &format!(
                "The update last found main at {} live, in {}{same}\n",
                s("main"),
                s("running")
            );
        }
        None => {
            out += &format!(
                "The update has found nothing live yet ({} isn't there)\n",
                live.display()
            )
        }
    }
    Ok(out)
}

/// `oom`: the kernel's OOM kills and systemd-oomd's since `since`, from
/// every boot the journal keeps in that span.
pub fn oom(m: &Machine, since: i64) -> Result<String, String> {
    let s = format!("@{since}");
    let jctl = &m.cfg.tools.journalctl;
    // The kernel's messages by their transport, not `-k`: that one means
    // this boot's only, and a kill is often what came before a reboot.
    let mut kernel = vec![
        "_TRANSPORT=kernel",
        "--since",
        &s,
        "-g",
        "oom-kill:|Out of memory|Memory cgroup out of memory",
    ];
    kernel.extend(JOURNAL);
    let mut oomd = vec!["-u", "systemd-oomd.service", "--since", &s, "-g", "Killed"];
    oomd.extend(JOURNAL);
    let k = m.output(jctl, &kernel, false, &[1]);
    let o = m.output(jctl, &oomd, false, &[1]);
    Ok(format!(
        "Since {}, the kernel's OOM kills:\n{}\n\nsystemd-oomd's:\n{}\n",
        utc(since),
        k.map_or_else(|e| e, |s| or_none(&s)),
        o.map_or_else(|e| e, |s| or_none(&s)),
    ))
}
